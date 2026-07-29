#include "nxp_host_ffi.h"

#include "fsl_clock.h"
#include "usb_host_config.h"
#include "usb.h"
#include "usb_host.h"
#include "usb_host_hid.h"
#include "usb_phy.h"

#define HOST_CONTROLLER_ID ((uint8_t)kUSB_ControllerEhci1)
#define EVENT_QUEUE_CAPACITY (32U)
#define REPORT_QUEUE_CAPACITY (32U)
#define REPORT_DATA_CAPACITY (64U)
#define HID_RX_BUFFER_SIZE (64U)
#define HID_REPORT_DESCRIPTOR_CAPACITY (512U)
#define MAX_HID_INTERFACES (6U)

typedef struct
{
    uint8_t occupied;
    uint8_t openStarted;
    uint8_t openCallbackSeen;
    uint8_t speed;
    uint8_t address;
    uint8_t hubAddress;
    uint8_t hubPort;
    uint8_t interfaceIndex;
    uint8_t interfaceNumber;
    uint8_t interfaceSubclass;
    uint8_t interfaceProtocol;
    uint8_t endpointAddress;
    uint8_t interval;
    uint16_t vid;
    uint16_t pid;
    uint16_t packetSize;
    uint16_t reportDescriptorLength;
    volatile uint16_t reportDescriptorActualLength;
    usb_device_handle deviceHandle;
    usb_host_interface_handle interfaceHandle;
    usb_host_class_handle classHandle;
    volatile uint8_t receiveArmed;
} hid_slot_t;

static usb_host_handle s_hostHandle;
static hid_slot_t s_hidSlots[MAX_HID_INTERFACES];
static hid_slot_t *s_openingSlot;
__attribute__((section(".usb_dma.hid_rx"), aligned(32)))
static uint8_t s_hidRxBuffers[MAX_HID_INTERFACES][HID_RX_BUFFER_SIZE];
__attribute__((section(".usb_dma.hid_report"), aligned(32)))
static uint8_t s_reportDescriptors[MAX_HID_INTERFACES][HID_REPORT_DESCRIPTOR_CAPACITY];
static nxp_host_event_t s_events[EVENT_QUEUE_CAPACITY];
static volatile uint8_t s_eventRead;
static volatile uint8_t s_eventWrite;
static nxp_host_report_t s_reports[REPORT_QUEUE_CAPACITY];
static volatile uint8_t s_reportRead;
static volatile uint8_t s_reportWrite;
static uint32_t s_reportSequence;

static void HidReceiveCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status);
static void StartNextHidInterface(void);

static void PushEvent(const nxp_host_event_t *event)
{
    uint8_t next = (uint8_t)((s_eventWrite + 1U) % EVENT_QUEUE_CAPACITY);
    if (next != s_eventRead)
    {
        s_events[s_eventWrite] = *event;
        s_eventWrite           = next;
    }
}

static void PushReport(const nxp_host_report_t *report)
{
    uint8_t next = (uint8_t)((s_reportWrite + 1U) % REPORT_QUEUE_CAPACITY);
    if (next != s_reportRead)
    {
        s_reports[s_reportWrite] = *report;
        s_reportWrite           = next;
    }
}

static uint16_t FindReportDescriptorLength(const usb_host_interface_t *interface)
{
    const uint8_t *descriptor = interface->interfaceExtension;
    uint16_t remaining        = interface->interfaceExtensionLength;

    while ((descriptor != NULL) && (remaining >= 2U))
    {
        uint8_t length = descriptor[0];
        uint8_t type   = descriptor[1];
        if ((length < 2U) || (length > remaining))
        {
            return 0U;
        }
        if ((type == USB_DESCRIPTOR_TYPE_HID) && (length >= 9U))
        {
            uint8_t descriptorCount = descriptor[5];
            uint8_t index;
            for (index = 0U; index < descriptorCount; ++index)
            {
                uint16_t offset = (uint16_t)(6U + ((uint16_t)index * 3U));
                if ((offset + 3U) > length)
                {
                    return 0U;
                }
                if (descriptor[offset] == USB_DESCRIPTOR_TYPE_HID_REPORT)
                {
                    return (uint16_t)descriptor[offset + 1U] |
                           ((uint16_t)descriptor[offset + 2U] << 8U);
                }
            }
        }
        descriptor += length;
        remaining = (uint16_t)(remaining - length);
    }
    return 0U;
}

static void FillSlotEvent(nxp_host_event_t *event, const hid_slot_t *slot)
{
    event->speed             = slot->speed;
    event->address           = slot->address;
    event->hubAddress        = slot->hubAddress;
    event->hubPort           = slot->hubPort;
    event->vid               = slot->vid;
    event->pid               = slot->pid;
    event->interfaceNumber   = slot->interfaceNumber;
    event->interfaceSubclass = slot->interfaceSubclass;
    event->interfaceProtocol = slot->interfaceProtocol;
    event->interfaceIndex    = slot->interfaceIndex;
    event->endpointAddress   = slot->endpointAddress;
    event->interval          = slot->interval;
    event->maxPacketSize     = slot->packetSize;
}

static void PushDescriptorEvent(hid_slot_t *slot, usb_status_t status, uint32_t descriptorLength)
{
    nxp_host_event_t descriptorEvent = {0};

    if (descriptorLength > HID_REPORT_DESCRIPTOR_CAPACITY)
    {
        descriptorLength = HID_REPORT_DESCRIPTOR_CAPACITY;
    }
    if (status == kStatus_USB_Success)
    {
        slot->reportDescriptorActualLength = (uint16_t)descriptorLength;
    }
    descriptorEvent.kind          = NXP_HOST_EVENT_REPORT_DESCRIPTOR;
    descriptorEvent.status        = (uint8_t)status;
    descriptorEvent.maxPacketSize = (uint16_t)descriptorLength;
    FillSlotEvent(&descriptorEvent, slot);
    descriptorEvent.maxPacketSize = (uint16_t)descriptorLength;
    PushEvent(&descriptorEvent);
}

static void StartHidReceive(hid_slot_t *slot)
{
    nxp_host_event_t readyEvent = {0};
    usb_status_t receiveStatus  = kStatus_USB_Error;

    if ((slot->classHandle != NULL) && (slot->packetSize != 0U) &&
        (slot->packetSize <= HID_RX_BUFFER_SIZE))
    {
        receiveStatus = USB_HostHidRecv(slot->classHandle,
                                        s_hidRxBuffers[slot->interfaceIndex],
                                        slot->packetSize,
                                        HidReceiveCallback,
                                        slot);
        if (receiveStatus == kStatus_USB_Success)
        {
            slot->receiveArmed = 1U;
        }
    }
    readyEvent.kind   = NXP_HOST_EVENT_HID_READY;
    readyEvent.status = (uint8_t)receiveStatus;
    FillSlotEvent(&readyEvent, slot);
    PushEvent(&readyEvent);
}

static void HidDescriptorCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    hid_slot_t *slot = (hid_slot_t *)param;
    (void)data;
    if (slot->occupied != 0U)
    {
        PushDescriptorEvent(slot, status, dataLen);
        if (s_openingSlot == slot)
        {
            s_openingSlot = NULL;
        }
        StartNextHidInterface();
    }
}

static void HidReceiveCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    nxp_host_report_t report = {0};
    hid_slot_t *slot = (hid_slot_t *)param;
    uint32_t index;
    uint32_t length = dataLen;

    /* The NXP class frees this transfer after returning from the callback.
     * Defer requeueing to nxp_host_task(), where the transfer object is back
     * in the pool and concurrent control requests cannot starve the requeue. */
    slot->receiveArmed = 0U;

    if (slot->occupied == 0U)
    {
        return;
    }

    if (length > REPORT_DATA_CAPACITY)
    {
        length = REPORT_DATA_CAPACITY;
    }
    if ((status == kStatus_USB_Success) && (length != 0U))
    {
        report.sequence = ++s_reportSequence;
        report.length   = (uint8_t)length;
        report.status   = (uint8_t)status;
        report.interfaceNumber = slot->interfaceNumber;
        report.interfaceIndex  = slot->interfaceIndex;
        for (index = 0U; index < length; ++index)
        {
            report.data[index] = data[index];
        }
        PushReport(&report);
    }
}

static void HidInterfaceCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    hid_slot_t *slot = (hid_slot_t *)param;
    usb_status_t descriptorStatus;
    (void)data;
    (void)dataLen;

    slot->openCallbackSeen = 1U;

    if (status != kStatus_USB_Success)
    {
        nxp_host_event_t readyEvent = {0};
        readyEvent.kind             = NXP_HOST_EVENT_HID_READY;
        readyEvent.status           = (uint8_t)status;
        FillSlotEvent(&readyEvent, slot);
        PushEvent(&readyEvent);
        PushDescriptorEvent(slot, status, 0U);
        if (s_openingSlot == slot)
        {
            s_openingSlot = NULL;
        }
        StartNextHidInterface();
        return;
    }

    /* Preserve the known-good receive timing: queue Interrupt IN as soon as
     * SET_INTERFACE completes. The report-descriptor control request uses a
     * different pipe and may run concurrently. */
    StartHidReceive(slot);

    if ((slot->classHandle != NULL) && (slot->reportDescriptorLength != 0U) &&
        (slot->reportDescriptorLength <= HID_REPORT_DESCRIPTOR_CAPACITY))
    {
        descriptorStatus = USB_HostHidGetReportDescriptor(slot->classHandle,
                                                          s_reportDescriptors[slot->interfaceIndex],
                                                          slot->reportDescriptorLength,
                                                          HidDescriptorCallback,
                                                          slot);
        if (descriptorStatus == kStatus_USB_Success)
        {
            return;
        }
        PushDescriptorEvent(slot, descriptorStatus, 0U);
        if (s_openingSlot == slot)
        {
            s_openingSlot = NULL;
        }
        StartNextHidInterface();
        return;
    }
    PushDescriptorEvent(slot, kStatus_USB_Error, 0U);
    if (s_openingSlot == slot)
    {
        s_openingSlot = NULL;
    }
    StartNextHidInterface();
}

static void StartNextHidInterface(void)
{
    uint8_t index;
    if (s_openingSlot != NULL)
    {
        return;
    }
    for (index = 0U; index < MAX_HID_INTERFACES; ++index)
    {
        hid_slot_t *slot = &s_hidSlots[index];
        usb_host_interface_t *interface;
        usb_status_t status;
        if ((slot->occupied == 0U) || (slot->openStarted != 0U))
        {
            continue;
        }
        slot->openStarted = 1U;
        slot->openCallbackSeen = 0U;
        s_openingSlot = slot;
        interface = (usb_host_interface_t *)slot->interfaceHandle;
        status = USB_HostHidInit(slot->deviceHandle, &slot->classHandle);
        if (status == kStatus_USB_Success)
        {
            status = USB_HostHidSetInterface(slot->classHandle,
                                             slot->interfaceHandle,
                                             interface->interfaceDesc->bAlternateSetting,
                                             HidInterfaceCallback,
                                             slot);
        }
        /* Alternate setting zero invokes the callback synchronously, including
         * on a pipe-open failure. The callback already advances the queue, so
         * do not emit the same failure a second time here. */
        if (slot->openCallbackSeen != 0U)
        {
            return;
        }
        if (status != kStatus_USB_Success)
        {
            nxp_host_event_t readyEvent = {0};
            readyEvent.kind             = NXP_HOST_EVENT_HID_READY;
            readyEvent.status           = (uint8_t)status;
            FillSlotEvent(&readyEvent, slot);
            PushEvent(&readyEvent);
            PushDescriptorEvent(slot, status, 0U);
            s_openingSlot = NULL;
            continue;
        }
        return;
    }
}

static uint32_t GetInfo(usb_device_handle device, usb_host_dev_info_t code)
{
    uint32_t value = 0U;
    (void)USB_HostHelperGetPeripheralInformation(device, (uint32_t)code, &value);
    return value;
}

static void InitRunClock(void)
{
    const clock_arm_pll_config_t armPll = {
        .loopDivider = 88U,
        .src = kCLOCK_PllClkSrc24M,
    };

    CLOCK_SetXtalFreq(24000000U);
    CLOCK_SetMux(kCLOCK_PeriphClk2Mux, 1U);
    CLOCK_SetDiv(kCLOCK_PeriphClk2Div, 0U);
    CLOCK_SetMux(kCLOCK_PeriphMux, 1U);

    DCDC->REG3 = (DCDC->REG3 & ~DCDC_REG3_TRG_MASK) | DCDC_REG3_TRG(0x12U);
    while ((DCDC->REG0 & DCDC_REG0_STS_DC_OK_MASK) == 0U)
    {
    }

    CLOCK_InitArmPll(&armPll);
    CLOCK_SetDiv(kCLOCK_AhbDiv, 0U);
    CLOCK_SetDiv(kCLOCK_IpgDiv, 3U);
    CLOCK_SetDiv(kCLOCK_ArmDiv, 1U);
    CLOCK_SetMux(kCLOCK_PrePeriphMux, 3U);
    CLOCK_SetMux(kCLOCK_PeriphMux, 0U);
    SystemCoreClock = 528000000U;
}

static usb_status_t HostEvent(usb_device_handle device,
                              usb_host_configuration_handle configurationHandle,
                              uint32_t eventCode)
{
    usb_host_configuration_t *configuration;
    uint8_t interfaceIndex;
    uint8_t endpointIndex;

    switch ((usb_host_event_t)(eventCode & 0xFFFFU))
    {
        case kUSB_HostEventAttach:
        {
            uint8_t hidFound = 0U;
            configuration = (usb_host_configuration_t *)configurationHandle;
            for (interfaceIndex = 0U; interfaceIndex < configuration->interfaceCount; ++interfaceIndex)
            {
                usb_host_interface_t *interface = &configuration->interfaceList[interfaceIndex];
                if (interface->interfaceDesc->bInterfaceClass == USB_HOST_HID_CLASS_CODE)
                {
                    hidFound = 1U;
                    break;
                }
            }
            if (hidFound == 0U)
            {
                return kStatus_USB_NotSupported;
            }
            return kStatus_USB_Success;
        }

        case kUSB_HostEventEnumerationDone:
            {
                configuration = (usb_host_configuration_t *)configurationHandle;
                for (interfaceIndex = 0U; interfaceIndex < configuration->interfaceCount;
                     ++interfaceIndex)
                {
                    usb_host_interface_t *interface = &configuration->interfaceList[interfaceIndex];
                    hid_slot_t *slot = NULL;
                    nxp_host_event_t event = {0};
                    uint8_t slotIndex;
                    if (interface->interfaceDesc->bInterfaceClass != USB_HOST_HID_CLASS_CODE)
                    {
                        continue;
                    }
                    for (slotIndex = 0U; slotIndex < MAX_HID_INTERFACES; ++slotIndex)
                    {
                        if (s_hidSlots[slotIndex].occupied == 0U)
                        {
                            slot = &s_hidSlots[slotIndex];
                            break;
                        }
                    }
                    if (slot == NULL)
                    {
                        break;
                    }
                    *slot = (hid_slot_t){0};
                    slot->deviceHandle = device;
                    slot->speed = (uint8_t)GetInfo(device, kUSB_HostGetDeviceSpeed);
                    slot->address = (uint8_t)GetInfo(device, kUSB_HostGetDeviceAddress);
                    slot->hubAddress = (uint8_t)GetInfo(device, kUSB_HostGetDeviceHubNumber);
                    slot->hubPort = (uint8_t)GetInfo(device, kUSB_HostGetDevicePortNumber);
                    slot->vid = (uint16_t)GetInfo(device, kUSB_HostGetDeviceVID);
                    slot->pid = (uint16_t)GetInfo(device, kUSB_HostGetDevicePID);
                    slot->interfaceIndex = slotIndex;
                    slot->interfaceNumber = interface->interfaceDesc->bInterfaceNumber;
                    slot->interfaceSubclass = interface->interfaceDesc->bInterfaceSubClass;
                    slot->interfaceProtocol = interface->interfaceDesc->bInterfaceProtocol;
                    slot->interfaceHandle = interface;
                    slot->reportDescriptorLength = FindReportDescriptorLength(interface);
                    for (endpointIndex = 0U; endpointIndex < interface->epCount; ++endpointIndex)
                    {
                        usb_descriptor_endpoint_t *ep = interface->epList[endpointIndex].epDesc;
                        if (((ep->bmAttributes & USB_DESCRIPTOR_ENDPOINT_ATTRIBUTE_TYPE_MASK) ==
                             USB_ENDPOINT_INTERRUPT) &&
                            ((ep->bEndpointAddress & USB_DESCRIPTOR_ENDPOINT_ADDRESS_DIRECTION_MASK) != 0U))
                        {
                            slot->endpointAddress = ep->bEndpointAddress;
                            slot->interval        = ep->bInterval;
                            slot->packetSize =
                                USB_SHORT_FROM_LITTLE_ENDIAN_ADDRESS(ep->wMaxPacketSize);
                            break;
                        }
                    }
                    if ((slot->endpointAddress == 0U) || (slot->packetSize == 0U))
                    {
                        *slot = (hid_slot_t){0};
                        continue;
                    }
                    slot->occupied = 1U;
                    event.kind = NXP_HOST_EVENT_HID_ATTACHED;
                    FillSlotEvent(&event, slot);
                    PushEvent(&event);
                    for (endpointIndex = 0U; endpointIndex < interface->epCount; ++endpointIndex)
                    {
                        usb_descriptor_endpoint_t *ep = interface->epList[endpointIndex].epDesc;
                        nxp_host_event_t endpointEvent = {0};
                        endpointEvent.kind = NXP_HOST_EVENT_ENDPOINT;
                        endpointEvent.status = ep->bmAttributes;
                        FillSlotEvent(&endpointEvent, slot);
                        endpointEvent.endpointAddress = ep->bEndpointAddress;
                        endpointEvent.interval = ep->bInterval;
                        endpointEvent.maxPacketSize =
                            USB_SHORT_FROM_LITTLE_ENDIAN_ADDRESS(ep->wMaxPacketSize);
                        PushEvent(&endpointEvent);
                    }
                }
                StartNextHidInterface();
            }
            return kStatus_USB_Success;

        case kUSB_HostEventDetach:
            {
                uint8_t index;
                for (index = 0U; index < MAX_HID_INTERFACES; ++index)
                {
                    hid_slot_t *slot = &s_hidSlots[index];
                    nxp_host_event_t event = {0};
                    usb_host_class_handle classHandle = slot->classHandle;
                    if ((slot->occupied == 0U) || (slot->deviceHandle != device))
                    {
                        continue;
                    }
                    event.kind = NXP_HOST_EVENT_DETACHED;
                    FillSlotEvent(&event, slot);
                    if (s_openingSlot == slot)
                    {
                        s_openingSlot = NULL;
                    }
                    slot->occupied = 0U;
                    slot->receiveArmed = 0U;
                    slot->classHandle = NULL;
                    PushEvent(&event);
                    if (classHandle != NULL)
                    {
                        (void)USB_HostHidDeinit(device, classHandle);
                    }
                    *slot = (hid_slot_t){0};
                }
                StartNextHidInterface();
            }
            return kStatus_USB_Success;

        case kUSB_HostEventEnumerationFail:
        {
            nxp_host_event_t event = {0};
            event.kind             = NXP_HOST_EVENT_ENUMERATION_FAILED;
            event.status           = (uint8_t)(eventCode >> 16U);
            PushEvent(&event);
            return kStatus_USB_Success;
        }

        default:
            return kStatus_USB_Success;
    }
}

int32_t nxp_host_init(void)
{
    usb_phy_config_struct_t phyConfig = {0x0CU, 0x06U, 0x06U};

    InitRunClock();
    (void)CLOCK_EnableUsbhs1PhyPllClock(kCLOCK_Usbphy480M, 480000000U);
    (void)CLOCK_EnableUsbhs1Clock(kCLOCK_Usb480M, 480000000U);
    (void)USB_EhciPhyInit(HOST_CONTROLLER_ID, 24000000U, &phyConfig);

    return (int32_t)USB_HostInit(HOST_CONTROLLER_ID, &s_hostHandle, HostEvent);
}

void nxp_host_task(void)
{
    uint8_t index;
    USB_HostEhciTaskFunction(s_hostHandle);
    for (index = 0U; index < MAX_HID_INTERFACES; ++index)
    {
        hid_slot_t *slot = &s_hidSlots[index];
        if ((slot->occupied != 0U) && (slot->classHandle != NULL) &&
            (slot->packetSize != 0U) && (slot->packetSize <= HID_RX_BUFFER_SIZE) &&
            (slot->receiveArmed == 0U))
        {
            usb_status_t status = USB_HostHidRecv(slot->classHandle,
                                                  s_hidRxBuffers[index],
                                                  slot->packetSize,
                                                  HidReceiveCallback,
                                                  slot);
            if (status == kStatus_USB_Success)
            {
                slot->receiveArmed = 1U;
            }
        }
    }
}

void nxp_host_irq(void)
{
    if (s_hostHandle != NULL)
    {
        USB_HostEhciIsrFunction(s_hostHandle);
    }
}

int32_t nxp_host_pop_event(nxp_host_event_t *event)
{
    if ((event == NULL) || (s_eventRead == s_eventWrite))
    {
        return 0;
    }

    *event      = s_events[s_eventRead];
    s_eventRead = (uint8_t)((s_eventRead + 1U) % EVENT_QUEUE_CAPACITY);
    return 1;
}

int32_t nxp_host_pop_report(nxp_host_report_t *report)
{
    if ((report == NULL) || (s_reportRead == s_reportWrite))
    {
        return 0;
    }

    *report       = s_reports[s_reportRead];
    s_reportRead  = (uint8_t)((s_reportRead + 1U) % REPORT_QUEUE_CAPACITY);
    return 1;
}

int32_t nxp_host_copy_report_descriptor(uint8_t interfaceIndex, uint8_t *buffer, uint16_t capacity)
{
    hid_slot_t *slot;
    uint16_t length;
    uint16_t index;
    if (interfaceIndex >= MAX_HID_INTERFACES)
    {
        return 0;
    }
    slot = &s_hidSlots[interfaceIndex];
    length = slot->reportDescriptorActualLength;
    if ((slot->occupied == 0U) || (buffer == NULL) || (length == 0U) || (capacity < length))
    {
        return 0;
    }
    for (index = 0U; index < length; ++index)
    {
        buffer[index] = s_reportDescriptors[interfaceIndex][index];
    }
    return (int32_t)length;
}

int32_t nxp_device_init_clocks(void)
{
    /* Match imxrt-hal's restart sequence. NXP's USB1 helper only checks
     * ENABLE; after reset-halt the PLL may still be enabled while POWER,
     * LOCK, or BYPASS are not in a usable state. */
    CCM_ANALOG->PLL_USB1_SET = CCM_ANALOG_PLL_USB1_ENABLE_MASK;
    CCM_ANALOG->PLL_USB1_SET = CCM_ANALOG_PLL_USB1_POWER_MASK;
    while ((CCM_ANALOG->PLL_USB1 & CCM_ANALOG_PLL_USB1_LOCK_MASK) == 0U)
    {
    }
    CCM_ANALOG->PLL_USB1_CLR = CCM_ANALOG_PLL_USB1_BYPASS_MASK;
    CCM_ANALOG->PLL_USB1_SET = CCM_ANALOG_PLL_USB1_EN_USB_CLKS_MASK;

    USBPHY1->CTRL &= ~USBPHY_CTRL_SFTRST_MASK;
    USBPHY1->CTRL &= ~USBPHY_CTRL_CLKGATE_MASK;
    USBPHY1->PWD = 0U;

    PMU->REG_3P0 = (PMU->REG_3P0 & (~PMU_REG_3P0_OUTPUT_TRG_MASK)) |
                   PMU_REG_3P0_OUTPUT_TRG(0x17U) | PMU_REG_3P0_ENABLE_LINREG_MASK;

    /* imxrt-usbd owns the USB1 core reset. Only provide its documented
     * prerequisites: the 480 MHz PLL, powered PHY / 3V regulator, and
     * peripheral gate. */
    CCM->CCGR6 |= CCM_CCGR6_CG0_MASK;
    return 0;
}

uint32_t nxp_core_clock_hz(void)
{
    SystemCoreClockUpdate();
    return SystemCoreClock;
}
