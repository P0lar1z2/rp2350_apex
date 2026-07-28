#ifndef RT1052_NXP_HOST_FFI_H
#define RT1052_NXP_HOST_FFI_H

#include <stdint.h>

enum
{
    NXP_HOST_EVENT_NONE = 0,
    NXP_HOST_EVENT_HID_ATTACHED,
    NXP_HOST_EVENT_DETACHED,
    NXP_HOST_EVENT_ENUMERATION_FAILED,
    NXP_HOST_EVENT_HID_READY,
    NXP_HOST_EVENT_REPORT_DESCRIPTOR,
};

typedef struct
{
    uint8_t kind;
    uint8_t status;
    uint8_t speed;
    uint8_t address;
    uint8_t hubAddress;
    uint8_t hubPort;
    uint8_t endpointAddress;
    uint8_t interval;
    uint16_t vid;
    uint16_t pid;
    uint16_t maxPacketSize;
    uint8_t interfaceNumber;
    uint8_t interfaceSubclass;
    uint8_t interfaceProtocol;
    uint8_t reserved;
} nxp_host_event_t;

typedef struct
{
    uint32_t sequence;
    uint8_t length;
    uint8_t status;
    uint8_t data[64];
    uint16_t reserved;
} nxp_host_report_t;

_Static_assert(sizeof(nxp_host_event_t) == 18U, "nxp_host_event_t ABI changed");
_Static_assert(sizeof(nxp_host_report_t) == 72U, "nxp_host_report_t ABI changed");

int32_t nxp_host_init(void);
void nxp_host_task(void);
void nxp_host_irq(void);
int32_t nxp_host_pop_event(nxp_host_event_t *event);
int32_t nxp_host_pop_report(nxp_host_report_t *report);
int32_t nxp_host_copy_report_descriptor(uint8_t *buffer, uint16_t capacity);
int32_t nxp_device_init_clocks(void);

#endif
