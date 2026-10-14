/*
  VK_LAYER_KP2_present: Vulkan on screen, on KernelProject2.

  Mesa's lavapipe (Vulkan on the processor) can draw, but only presents to X11, Wayland, or
  nowhere: "headless" surfaces, whose presented pictures go no further. Our SDL makes headless
  surfaces for its windows. This layer, which the Vulkan loader puts in front of every program
  (an implicit layer), takes each picture as it's presented, copies it out of the swapchain
  image into memory it can read, and sends it to the window on /dev/window, then lets the
  present go on as before.

  Which window: SDL tells (KP2_VulkanSurfaceWindow, in our libSDL2); a program that made its
  headless surface itself gets a window of its own.
*/
#include <vulkan/vulkan.h>
#include <vulkan/vk_layer.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define EXPORT __attribute__((visibility("default")))
#define WINDOW_PIXELS 0x5701
#define WINDOW_TITLE 0x5702
#define WINDOW_BLIT 0x5703

struct blit {
    uint32_t x, y, w, h, stride, pad;
    uint64_t pixels;
};

/* Dispatchable handles start with the loader's dispatch table pointer: the key to our tables. */
static void *key(const void *handle)
{
    return *(void **)handle;
}

typedef struct {
    void *key;
    VkInstance instance;
    PFN_vkGetInstanceProcAddr gipa;
    PFN_vkDestroyInstance DestroyInstance;
    PFN_vkGetPhysicalDeviceMemoryProperties GetPhysicalDeviceMemoryProperties;
} Instance;

typedef struct {
    void *key;
    VkDevice device;
    VkPhysicalDeviceMemoryProperties memory;
    PFN_vkGetDeviceProcAddr gdpa;
    /* The loader's: makes a dispatchable handle we create (our command buffer) usable. */
    PFN_vkSetDeviceLoaderData set_loader_data;
    PFN_vkDestroyDevice DestroyDevice;
    PFN_vkGetDeviceQueue GetDeviceQueue;
    PFN_vkGetDeviceQueue2 GetDeviceQueue2;
    PFN_vkCreateSwapchainKHR CreateSwapchainKHR;
    PFN_vkDestroySwapchainKHR DestroySwapchainKHR;
    PFN_vkGetSwapchainImagesKHR GetSwapchainImagesKHR;
    PFN_vkQueuePresentKHR QueuePresentKHR;
    PFN_vkCreateBuffer CreateBuffer;
    PFN_vkDestroyBuffer DestroyBuffer;
    PFN_vkGetBufferMemoryRequirements GetBufferMemoryRequirements;
    PFN_vkAllocateMemory AllocateMemory;
    PFN_vkFreeMemory FreeMemory;
    PFN_vkBindBufferMemory BindBufferMemory;
    PFN_vkMapMemory MapMemory;
    PFN_vkCreateCommandPool CreateCommandPool;
    PFN_vkDestroyCommandPool DestroyCommandPool;
    PFN_vkAllocateCommandBuffers AllocateCommandBuffers;
    PFN_vkBeginCommandBuffer BeginCommandBuffer;
    PFN_vkEndCommandBuffer EndCommandBuffer;
    PFN_vkResetCommandBuffer ResetCommandBuffer;
    PFN_vkCmdPipelineBarrier CmdPipelineBarrier;
    PFN_vkCmdCopyImageToBuffer CmdCopyImageToBuffer;
    PFN_vkQueueSubmit QueueSubmit;
    PFN_vkCreateFence CreateFence;
    PFN_vkDestroyFence DestroyFence;
    PFN_vkWaitForFences WaitForFences;
    PFN_vkResetFences ResetFences;
} Device;

typedef struct {
    VkQueue queue;
    uint32_t family;
} Queue;

typedef struct {
    VkSwapchainKHR swapchain;
    Device *dev;
    VkImage images[16];
    uint32_t count;
    VkFormat format;
    VkExtent2D extent;
    VkBuffer buffer;
    VkDeviceMemory memory;
    uint32_t *pixels;
    VkCommandPool pool;
    uint32_t pool_family;
    VkCommandBuffer cmd;
    VkFence fence;
    int fd, own_fd;
} Swapchain;

#define MAX 64
static Instance instances[MAX];
static Device devices[MAX];
static Queue queues[MAX];
static Swapchain swapchains[MAX];
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

static Instance *find_instance(void *k)
{
    for (int i = 0; i < MAX; i++) {
        if (instances[i].key == k) return &instances[i];
    }
    return NULL;
}

static Device *find_device(void *k)
{
    for (int i = 0; i < MAX; i++) {
        if (devices[i].key == k) return &devices[i];
    }
    return NULL;
}

static Swapchain *find_swapchain(VkSwapchainKHR s)
{
    for (int i = 0; i < MAX; i++) {
        if (swapchains[i].swapchain == s && s != VK_NULL_HANDLE) return &swapchains[i];
    }
    return NULL;
}

/* ---- instances ---- */

static VKAPI_ATTR VkResult VKAPI_CALL CreateInstance(const VkInstanceCreateInfo *info, const VkAllocationCallbacks *alloc, VkInstance *out)
{
    VkLayerInstanceCreateInfo *chain = (VkLayerInstanceCreateInfo *)info->pNext;
    while (chain && !(chain->sType == VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO && chain->function == VK_LAYER_LINK_INFO)) {
        chain = (VkLayerInstanceCreateInfo *)chain->pNext;
    }
    if (!chain) return VK_ERROR_INITIALIZATION_FAILED;
    PFN_vkGetInstanceProcAddr gipa = chain->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    chain->u.pLayerInfo = chain->u.pLayerInfo->pNext;
    PFN_vkCreateInstance next = (PFN_vkCreateInstance)gipa(VK_NULL_HANDLE, "vkCreateInstance");
    VkResult r = next(info, alloc, out);
    if (r != VK_SUCCESS) return r;

    pthread_mutex_lock(&lock);
    Instance *in = find_instance(NULL);
    if (in) {
        in->key = key(*out);
        in->instance = *out;
        in->gipa = gipa;
        in->DestroyInstance = (PFN_vkDestroyInstance)gipa(*out, "vkDestroyInstance");
        in->GetPhysicalDeviceMemoryProperties = (PFN_vkGetPhysicalDeviceMemoryProperties)gipa(*out, "vkGetPhysicalDeviceMemoryProperties");
    }
    pthread_mutex_unlock(&lock);
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL DestroyInstance(VkInstance instance, const VkAllocationCallbacks *alloc)
{
    if (!instance) return;
    Instance *in = find_instance(key(instance));
    if (!in) return;
    PFN_vkDestroyInstance next = in->DestroyInstance;
    memset(in, 0, sizeof *in);
    next(instance, alloc);
}

/* ---- devices ---- */

#define LOAD(d, name) (d)->name = (PFN_vk##name)(d)->gdpa((d)->device, "vk" #name)

static VKAPI_ATTR VkResult VKAPI_CALL CreateDevice(VkPhysicalDevice gpu, const VkDeviceCreateInfo *info, const VkAllocationCallbacks *alloc, VkDevice *out)
{
    VkLayerDeviceCreateInfo *chain = (VkLayerDeviceCreateInfo *)info->pNext;
    while (chain && !(chain->sType == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO && chain->function == VK_LAYER_LINK_INFO)) {
        chain = (VkLayerDeviceCreateInfo *)chain->pNext;
    }
    if (!chain) return VK_ERROR_INITIALIZATION_FAILED;
    PFN_vkGetInstanceProcAddr gipa = chain->u.pLayerInfo->pfnNextGetInstanceProcAddr;
    PFN_vkGetDeviceProcAddr gdpa = chain->u.pLayerInfo->pfnNextGetDeviceProcAddr;
    chain->u.pLayerInfo = chain->u.pLayerInfo->pNext;
    PFN_vkSetDeviceLoaderData set_loader_data = NULL;
    for (VkLayerDeviceCreateInfo *c = (VkLayerDeviceCreateInfo *)info->pNext; c; c = (VkLayerDeviceCreateInfo *)c->pNext) {
        if (c->sType == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO && c->function == VK_LOADER_DATA_CALLBACK) {
            set_loader_data = c->u.pfnSetDeviceLoaderData;
        }
    }
    PFN_vkCreateDevice next = (PFN_vkCreateDevice)gipa(VK_NULL_HANDLE, "vkCreateDevice");
    VkResult r = next(gpu, info, alloc, out);
    if (r != VK_SUCCESS) return r;

    pthread_mutex_lock(&lock);
    Device *d = find_device(NULL);
    Instance *in = find_instance(key(gpu));
    if (d) {
        memset(d, 0, sizeof *d);
        d->key = key(*out);
        d->device = *out;
        d->gdpa = gdpa;
        d->set_loader_data = set_loader_data;
        if (in) in->GetPhysicalDeviceMemoryProperties(gpu, &d->memory);
        LOAD(d, DestroyDevice);
        LOAD(d, GetDeviceQueue);
        LOAD(d, GetDeviceQueue2);
        LOAD(d, CreateSwapchainKHR);
        LOAD(d, DestroySwapchainKHR);
        LOAD(d, GetSwapchainImagesKHR);
        LOAD(d, QueuePresentKHR);
        LOAD(d, CreateBuffer);
        LOAD(d, DestroyBuffer);
        LOAD(d, GetBufferMemoryRequirements);
        LOAD(d, AllocateMemory);
        LOAD(d, FreeMemory);
        LOAD(d, BindBufferMemory);
        LOAD(d, MapMemory);
        LOAD(d, CreateCommandPool);
        LOAD(d, DestroyCommandPool);
        LOAD(d, AllocateCommandBuffers);
        LOAD(d, BeginCommandBuffer);
        LOAD(d, EndCommandBuffer);
        LOAD(d, ResetCommandBuffer);
        LOAD(d, CmdPipelineBarrier);
        LOAD(d, CmdCopyImageToBuffer);
        LOAD(d, QueueSubmit);
        LOAD(d, CreateFence);
        LOAD(d, DestroyFence);
        LOAD(d, WaitForFences);
        LOAD(d, ResetFences);
    }
    pthread_mutex_unlock(&lock);
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL DestroyDevice(VkDevice device, const VkAllocationCallbacks *alloc)
{
    if (!device) return;
    Device *d = find_device(key(device));
    if (!d) return;
    PFN_vkDestroyDevice next = d->DestroyDevice;
    pthread_mutex_lock(&lock);
    for (int i = 0; i < MAX; i++) {
        if (queues[i].queue && key(queues[i].queue) == d->key) memset(&queues[i], 0, sizeof queues[i]);
    }
    memset(d, 0, sizeof *d);
    pthread_mutex_unlock(&lock);
    next(device, alloc);
}

/* Which family each queue is from: the copy at present time runs on the presenting queue. */
static void remember_queue(VkQueue q, uint32_t family)
{
    pthread_mutex_lock(&lock);
    for (int i = 0; i < MAX; i++) {
        if (queues[i].queue == q) break;
        if (!queues[i].queue) {
            queues[i].queue = q;
            queues[i].family = family;
            break;
        }
    }
    pthread_mutex_unlock(&lock);
}

static VKAPI_ATTR void VKAPI_CALL GetDeviceQueue(VkDevice device, uint32_t family, uint32_t index, VkQueue *q)
{
    find_device(key(device))->GetDeviceQueue(device, family, index, q);
    remember_queue(*q, family);
}

static VKAPI_ATTR void VKAPI_CALL GetDeviceQueue2(VkDevice device, const VkDeviceQueueInfo2 *info, VkQueue *q)
{
    find_device(key(device))->GetDeviceQueue2(device, info, q);
    remember_queue(*q, info->queueFamilyIndex);
}

/* ---- swapchains ---- */

static int window_for(VkSurfaceKHR surface, VkExtent2D extent, int *own)
{
    int (*sdl)(uint64_t) = (int (*)(uint64_t))dlsym(RTLD_DEFAULT, "KP2_VulkanSurfaceWindow");
    int fd = sdl ? sdl((uint64_t)surface) : -1;
    *own = 0;
    if (fd >= 0) return fd;
    fd = open("/dev/window", O_RDWR | O_CLOEXEC);
    if (fd >= 0) {
        uint32_t size[2] = { extent.width, extent.height };
        ioctl(fd, WINDOW_PIXELS, size);
        ioctl(fd, WINDOW_TITLE, "Vulkan");
        *own = 1;
    }
    return fd;
}

static int32_t memory_type(Device *d, uint32_t bits, VkMemoryPropertyFlags want)
{
    for (uint32_t i = 0; i < d->memory.memoryTypeCount; i++) {
        if ((bits & (1u << i)) && (d->memory.memoryTypes[i].propertyFlags & want) == want) return (int32_t)i;
    }
    return -1;
}

static void release(Swapchain *s)
{
    Device *d = s->dev;
    if (s->fence) d->DestroyFence(d->device, s->fence, NULL);
    if (s->pool) d->DestroyCommandPool(d->device, s->pool, NULL);
    if (s->buffer) d->DestroyBuffer(d->device, s->buffer, NULL);
    if (s->memory) d->FreeMemory(d->device, s->memory, NULL);
    if (s->own_fd && s->fd >= 0) close(s->fd);
    memset(s, 0, sizeof *s);
}

static VKAPI_ATTR VkResult VKAPI_CALL CreateSwapchainKHR(VkDevice device, const VkSwapchainCreateInfoKHR *info, const VkAllocationCallbacks *alloc, VkSwapchainKHR *out)
{
    Device *d = find_device(key(device));
    VkSwapchainCreateInfoKHR copy = *info;
    copy.imageUsage |= VK_IMAGE_USAGE_TRANSFER_SRC_BIT; /* so we can read the pictures back */
    VkResult r = d->CreateSwapchainKHR(device, &copy, alloc, out);
    if (r != VK_SUCCESS) return r;

    pthread_mutex_lock(&lock);
    Swapchain *s = find_swapchain(VK_NULL_HANDLE);
    for (int i = 0; i < MAX && !s; i++) {
        if (!swapchains[i].swapchain) s = &swapchains[i];
    }
    pthread_mutex_unlock(&lock);
    if (!s) return VK_SUCCESS; /* too many: this one just won't show */
    memset(s, 0, sizeof *s);
    s->swapchain = *out;
    s->dev = d;
    s->format = info->imageFormat;
    s->extent = info->imageExtent;
    s->count = 16;
    d->GetSwapchainImagesKHR(device, *out, &s->count, s->images);

    /* Memory the processor can read, a whole picture's worth. */
    VkBufferCreateInfo bi = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
                              .size = (VkDeviceSize)s->extent.width * s->extent.height * 4,
                              .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT };
    VkMemoryRequirements req;
    if (d->CreateBuffer(device, &bi, NULL, &s->buffer) != VK_SUCCESS) goto fail;
    d->GetBufferMemoryRequirements(device, s->buffer, &req);
    int32_t type = memory_type(d, req.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
    VkMemoryAllocateInfo mi = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, .allocationSize = req.size, .memoryTypeIndex = (uint32_t)type };
    if (type < 0 || d->AllocateMemory(device, &mi, NULL, &s->memory) != VK_SUCCESS) goto fail;
    d->BindBufferMemory(device, s->buffer, s->memory, 0);
    if (d->MapMemory(device, s->memory, 0, VK_WHOLE_SIZE, 0, (void **)&s->pixels) != VK_SUCCESS) goto fail;
    VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
    if (d->CreateFence(device, &fi, NULL, &s->fence) != VK_SUCCESS) goto fail;
    s->fd = window_for(info->surface, s->extent, &s->own_fd);
    return VK_SUCCESS;
fail:
    release(s);
    return VK_SUCCESS;
}

static VKAPI_ATTR void VKAPI_CALL DestroySwapchainKHR(VkDevice device, VkSwapchainKHR swapchain, const VkAllocationCallbacks *alloc)
{
    Device *d = find_device(key(device));
    Swapchain *s = find_swapchain(swapchain);
    if (s) release(s);
    d->DestroySwapchainKHR(device, swapchain, alloc);
}

/* Records: the image becomes a copy source, is copied into the buffer, and goes back to being
   presentable. */
static int record(Swapchain *s, Device *d, uint32_t family, VkImage image)
{
    if (!s->pool || s->pool_family != family) {
        if (s->pool) d->DestroyCommandPool(d->device, s->pool, NULL);
        VkCommandPoolCreateInfo pi = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                       .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT, .queueFamilyIndex = family };
        if (d->CreateCommandPool(d->device, &pi, NULL, &s->pool) != VK_SUCCESS) return 0;
        VkCommandBufferAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO, .commandPool = s->pool,
                                           .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
        if (d->AllocateCommandBuffers(d->device, &ai, &s->cmd) != VK_SUCCESS) return 0;
        if (d->set_loader_data) {
            d->set_loader_data(d->device, s->cmd);
        } else {
            *(void **)s->cmd = *(void **)d->device; /* the same thing, by hand */
        }
        s->pool_family = family;
    }
    d->ResetCommandBuffer(s->cmd, 0);
    VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO, .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
    d->BeginCommandBuffer(s->cmd, &bi);
    VkImageMemoryBarrier b = { .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                               .srcAccessMask = VK_ACCESS_MEMORY_WRITE_BIT, .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
                               .oldLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR, .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                               .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED, .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                               .image = image, .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    d->CmdPipelineBarrier(s->cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b);
    VkBufferImageCopy region = { .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 },
                                 .imageExtent = { s->extent.width, s->extent.height, 1 } };
    d->CmdCopyImageToBuffer(s->cmd, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, s->buffer, 1, &region);
    b.srcAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    b.dstAccessMask = 0;
    b.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    b.newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    d->CmdPipelineBarrier(s->cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL, 0, NULL, 1, &b);
    d->EndCommandBuffer(s->cmd);
    return 1;
}

static void show(Swapchain *s)
{
    uint32_t n = s->extent.width * s->extent.height;
    /* The window's pixels are 0x00RRGGBB: BGRA images are that already; RGBA ones are swapped. */
    if (s->format == VK_FORMAT_R8G8B8A8_UNORM || s->format == VK_FORMAT_R8G8B8A8_SRGB) {
        for (uint32_t i = 0; i < n; i++) {
            uint32_t p = s->pixels[i];
            s->pixels[i] = (p & 0xff00ff00u) | (p & 0xff) << 16 | (p >> 16 & 0xff);
        }
    }
    struct blit b = { 0, 0, s->extent.width, s->extent.height, s->extent.width, 0, (uint64_t)(uintptr_t)s->pixels };
    ioctl(s->fd, WINDOW_BLIT, &b);
}

static VKAPI_ATTR VkResult VKAPI_CALL QueuePresentKHR(VkQueue queue, const VkPresentInfoKHR *info)
{
    Device *d = find_device(key(queue));
    uint32_t family = 0;
    for (int i = 0; i < MAX; i++) {
        if (queues[i].queue == queue) family = queues[i].family;
    }
    uint32_t waits = info->waitSemaphoreCount;
    VkPipelineStageFlags stages[16];
    for (uint32_t i = 0; i < 16; i++) stages[i] = VK_PIPELINE_STAGE_ALL_COMMANDS_BIT;
    for (uint32_t i = 0; i < info->swapchainCount; i++) {
        Swapchain *s = find_swapchain(info->pSwapchains[i]);
        if (!s || s->fd < 0 || !s->pixels || info->pImageIndices[i] >= s->count) continue;
        if (!record(s, d, family, s->images[info->pImageIndices[i]])) continue;
        /* The first copy waits for the drawing (the present's semaphores); the present itself
           then has nothing left to wait for. */
        VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                            .waitSemaphoreCount = waits <= 16 ? waits : 16, .pWaitSemaphores = info->pWaitSemaphores,
                            .pWaitDstStageMask = stages, .commandBufferCount = 1, .pCommandBuffers = &s->cmd };
        d->ResetFences(d->device, 1, &s->fence);
        if (d->QueueSubmit(queue, 1, &si, s->fence) != VK_SUCCESS) continue;
        waits = 0;
        d->WaitForFences(d->device, 1, &s->fence, VK_TRUE, UINT64_MAX);
        show(s);
    }
    VkPresentInfoKHR rest = *info;
    if (waits == 0) {
        rest.waitSemaphoreCount = 0;
        rest.pWaitSemaphores = NULL;
    }
    return d->QueuePresentKHR(queue, &rest);
}

/* ---- the layer's entry points ---- */

static PFN_vkVoidFunction ours(const char *name)
{
#define OURS(f) if (strcmp(name, "vk" #f) == 0) return (PFN_vkVoidFunction)f
    OURS(CreateInstance);
    OURS(DestroyInstance);
    OURS(CreateDevice);
    OURS(DestroyDevice);
    OURS(GetDeviceQueue);
    OURS(GetDeviceQueue2);
    OURS(CreateSwapchainKHR);
    OURS(DestroySwapchainKHR);
    OURS(QueuePresentKHR);
#undef OURS
    return NULL;
}

EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL KP2_present_GetDeviceProcAddr(VkDevice device, const char *name)
{
    PFN_vkVoidFunction f = ours(name);
    if (f && strcmp(name, "vkCreateInstance") && strcmp(name, "vkDestroyInstance") && strcmp(name, "vkCreateDevice")) return f;
    Device *d = find_device(key(device));
    return d ? d->gdpa(device, name) : NULL;
}

EXPORT VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL KP2_present_GetInstanceProcAddr(VkInstance instance, const char *name)
{
    PFN_vkVoidFunction f = ours(name);
    if (f) return f;
    if (strcmp(name, "vkGetDeviceProcAddr") == 0) return (PFN_vkVoidFunction)KP2_present_GetDeviceProcAddr;
    if (!instance) return NULL;
    Instance *in = find_instance(key(instance));
    return in ? in->gipa(instance, name) : NULL;
}

/* The loader asks for these by the names in the layer's manifest (present_layer.json). */
EXPORT VKAPI_ATTR VkResult VKAPI_CALL KP2_present_NegotiateLoaderLayerInterfaceVersion(VkNegotiateLayerInterface *v)
{
    if (v->loaderLayerInterfaceVersion > 2) v->loaderLayerInterfaceVersion = 2;
    v->pfnGetInstanceProcAddr = KP2_present_GetInstanceProcAddr;
    v->pfnGetDeviceProcAddr = KP2_present_GetDeviceProcAddr;
    v->pfnGetPhysicalDeviceProcAddr = NULL;
    return VK_SUCCESS;
}
