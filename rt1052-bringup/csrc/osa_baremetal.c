#include "fsl_os_abstraction.h"

#include <stddef.h>
#include <stdint.h>

#define NXP_USB_POOL_SIZE (96U * 1024U)

typedef struct
{
    volatile uint32_t flags;
    uint32_t autoClear;
} event_state_t;

__attribute__((section(".usb_dma.nxp_pool"), aligned(32)))
static uint8_t s_usbPool[NXP_USB_POOL_SIZE];
static uint32_t s_usbPoolUsed;

void *OSA_MemoryAllocateAlign(uint32_t length, uint32_t alignment)
{
    uint32_t start;
    uint32_t index;

    if ((alignment == 0U) || ((alignment & (alignment - 1U)) != 0U))
    {
        return NULL;
    }

    start = (s_usbPoolUsed + alignment - 1U) & ~(alignment - 1U);
    if ((start > NXP_USB_POOL_SIZE) || (length > (NXP_USB_POOL_SIZE - start)))
    {
        return NULL;
    }

    s_usbPoolUsed = start + length;
    for (index = 0U; index < length; ++index)
    {
        s_usbPool[start + index] = 0U;
    }
    return &s_usbPool[start];
}

void *OSA_MemoryAllocate(uint32_t length)
{
    return OSA_MemoryAllocateAlign(length, 8U);
}

void OSA_MemoryFree(void *pointer)
{
    (void)pointer;
}

void OSA_MemoryFreeAlign(void *pointer)
{
    (void)pointer;
}

osa_status_t OSA_MutexCreate(osa_mutex_handle_t handle)
{
    *(volatile uint32_t *)handle = 0U;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_MutexDestroy(osa_mutex_handle_t handle)
{
    (void)handle;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_MutexLock(osa_mutex_handle_t handle, uint32_t timeout)
{
    (void)handle;
    (void)timeout;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_MutexUnlock(osa_mutex_handle_t handle)
{
    (void)handle;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_EventCreate(osa_event_handle_t handle, uint8_t autoClear)
{
    event_state_t *event = (event_state_t *)handle;
    event->flags         = 0U;
    event->autoClear     = autoClear;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_EventDestroy(osa_event_handle_t handle)
{
    (void)handle;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_EventSet(osa_event_handle_t handle, osa_event_flags_t flags)
{
    event_state_t *event = (event_state_t *)handle;
    event->flags |= flags;
    return KOSA_StatusSuccess;
}

osa_status_t OSA_EventWait(osa_event_handle_t handle,
                           osa_event_flags_t flags,
                           uint8_t waitAll,
                           uint32_t timeout,
                           osa_event_flags_t *setFlags)
{
    event_state_t *event = (event_state_t *)handle;
    uint32_t state       = event->flags & flags;
    uint8_t ready        = waitAll ? (state == flags) : (state != 0U);
    (void)timeout;

    if (!ready)
    {
        *setFlags = 0U;
        return KOSA_StatusIdle;
    }
    *setFlags = state;
    if (event->autoClear != 0U)
    {
        event->flags &= ~state;
    }
    return KOSA_StatusSuccess;
}

void OSA_EnterCritical(uint32_t *state)
{
    __asm volatile("mrs %0, primask\n"
                   "cpsid i"
                   : "=r"(*state)
                   :
                   : "memory");
}

void OSA_ExitCritical(uint32_t state)
{
    __asm volatile("msr primask, %0" : : "r"(state) : "memory");
}
