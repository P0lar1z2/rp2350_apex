#ifndef RT1052_MINIMAL_FSL_OS_ABSTRACTION_H
#define RT1052_MINIMAL_FSL_OS_ABSTRACTION_H

#include <stdint.h>

typedef void *osa_mutex_handle_t;
typedef void *osa_event_handle_t;
typedef uint32_t osa_event_flags_t;

typedef enum _osa_status
{
    KOSA_StatusSuccess = 0,
    KOSA_StatusError   = 1,
    KOSA_StatusTimeout = 2,
    KOSA_StatusIdle    = 3,
} osa_status_t;

#define OSA_StatusSuccess KOSA_StatusSuccess
#define OSA_WAIT_TIMEOUT  (0U)
#define osaWaitForever_c  (0xFFFFFFFFU)
#define USE_RTOS          (0U)
#define OSA_MUTEX_HANDLE_SIZE (4U)
#define OSA_EVENT_HANDLE_SIZE (8U)

#define OSA_SR_ALLOC() uint32_t osaCurrentSr = 0U
#define OSA_ENTER_CRITICAL() OSA_EnterCritical(&osaCurrentSr)
#define OSA_EXIT_CRITICAL() OSA_ExitCritical(osaCurrentSr)

void *OSA_MemoryAllocate(uint32_t length);
void OSA_MemoryFree(void *pointer);
void *OSA_MemoryAllocateAlign(uint32_t length, uint32_t alignment);
void OSA_MemoryFreeAlign(void *pointer);

osa_status_t OSA_MutexCreate(osa_mutex_handle_t handle);
osa_status_t OSA_MutexDestroy(osa_mutex_handle_t handle);
osa_status_t OSA_MutexLock(osa_mutex_handle_t handle, uint32_t timeout);
osa_status_t OSA_MutexUnlock(osa_mutex_handle_t handle);

osa_status_t OSA_EventCreate(osa_event_handle_t handle, uint8_t autoClear);
osa_status_t OSA_EventDestroy(osa_event_handle_t handle);
osa_status_t OSA_EventSet(osa_event_handle_t handle, osa_event_flags_t flags);
osa_status_t OSA_EventWait(osa_event_handle_t handle,
                           osa_event_flags_t flags,
                           uint8_t waitAll,
                           uint32_t timeout,
                           osa_event_flags_t *setFlags);

void OSA_EnterCritical(uint32_t *state);
void OSA_ExitCritical(uint32_t state);

#endif
