#include "nxp_host_ffi.h"

#include "fsl_clock.h"
#include "usb_host_config.h"
#include "usb.h"
#include "usb_host.h"
#include "usb_host_hid.h"
#include "usb_phy.h"

#define HOST_CONTROLLER_ID ((uint8_t)kUSB_ControllerEhci1)
#define EVENT_QUEUE_CAPACITY (8U)
#define REPORT_QUEUE_CAPACITY (32U)
#define REPORT_DATA_CAPACITY (64U)
#define HID_RX_BUFFER_SIZE (64U)
#define HID_REPORT_DESCRIPTOR_CAPACITY (512U)

static usb_host_handle s_hostHandle;
static usb_device_handle s_hidDevice;
static usb_host_configuration_handle s_hidConfiguration;
static usb_host_interface_handle s_hidInterface;
static usb_host_class_handle s_hidClass;
static uint16_t s_hidPacketSize;
static uint16_t s_reportDescriptorLength;
static volatile uint16_t s_reportDescriptorActualLength;
__attribute__((section(".usb_dma.hid_rx"), aligned(32)))
static uint8_t s_hidRxBuffer[HID_RX_BUFFER_SIZE];
__attribute__((section(".usb_dma.hid_report"), aligned(32)))
static uint8_t s_reportDescriptor[HID_REPORT_DESCRIPTOR_CAPACITY];
static nxp_host_event_t s_events[EVENT_QUEUE_CAPACITY];
static volatile uint8_t s_eventRead;
static volatile uint8_t s_eventWrite;
static nxp_host_report_t s_reports[REPORT_QUEUE_CAPACITY];
static volatile uint8_t s_reportRead;
static volatile uint8_t s_reportWrite;
static uint32_t s_reportSequence;
static volatile uint8_t s_receiveArmed;

static void HidReceiveCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status);

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

static void PushDescriptorEvent(usb_status_t status, uint32_t descriptorLength)
{
    nxp_host_event_t descriptorEvent = {0};

    if (descriptorLength > HID_REPORT_DESCRIPTOR_CAPACITY)
    {
        descriptorLength = HID_REPORT_DESCRIPTOR_CAPACITY;
    }
    if (status == kStatus_USB_Success)
    {
        s_reportDescriptorActualLength = (uint16_t)descriptorLength;
    }
    descriptorEvent.kind          = NXP_HOST_EVENT_REPORT_DESCRIPTOR;
    descriptorEvent.status        = (uint8_t)status;
    descriptorEvent.maxPacketSize = (uint16_t)descriptorLength;
    PushEvent(&descriptorEvent);
}

static void StartHidReceive(void)
{
    nxp_host_event_t readyEvent = {0};
    usb_status_t receiveStatus  = kStatus_USB_Error;

    if ((s_hidClass != NULL) && (s_hidPacketSize != 0U) &&
        (s_hidPacketSize <= HID_RX_BUFFER_SIZE))
    {
        receiveStatus =
            USB_HostHidRecv(s_hidClass, s_hidRxBuffer, s_hidPacketSize, HidReceiveCallback, NULL);
        if (receiveStatus == kStatus_USB_Success)
        {
            s_receiveArmed = 1U;
        }
    }
    readyEvent.kind   = NXP_HOST_EVENT_HID_READY;
    readyEvent.status = (uint8_t)receiveStatus;
    PushEvent(&readyEvent);
}

static void HidDescriptorCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    (void)param;
    (void)data;
    PushDescriptorEvent(status, dataLen);
}

static void HidReceiveCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    nxp_host_report_t report = {0};
    uint32_t index;
    uint32_t length = dataLen;
    (void)param;

    /* The NXP class frees this transfer after returning from the callback.
     * Defer requeueing to nxp_host_task(), where the transfer object is back
     * in the pool and concurrent control requests cannot starve the requeue. */
    s_receiveArmed = 0U;

    if (length > REPORT_DATA_CAPACITY)
    {
        length = REPORT_DATA_CAPACITY;
    }
    report.sequence = ++s_reportSequence;
    report.length   = (uint8_t)length;
    report.status   = (uint8_t)status;
    for (index = 0U; index < length; ++index)
    {
        report.data[index] = data[index];
    }
    PushReport(&report);

}

static void HidInterfaceCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    usb_status_t descriptorStatus;
    (void)param;
    (void)data;
    (void)dataLen;

    if (status != kStatus_USB_Success)
    {
        PushDescriptorEvent(status, 0U);
        StartHidReceive();
        return;
    }

    /* Preserve the known-good receive timing: queue Interrupt IN as soon as
     * SET_INTERFACE completes. The report-descriptor control request uses a
     * different pipe and may run concurrently. */
    StartHidReceive();

    if ((s_hidClass != NULL) && (s_reportDescriptorLength != 0U) &&
        (s_reportDescriptorLength <= HID_REPORT_DESCRIPTOR_CAPACITY))
    {
        descriptorStatus = USB_HostHidGetReportDescriptor(s_hidClass,
                                                          s_reportDescriptor,
                                                          s_reportDescriptorLength,
                                                          HidDescriptorCallback,
                                                          NULL);
        if (descriptorStatus == kStatus_USB_Success)
        {
            return;
        }
        PushDescriptorEvent(descriptorStatus, 0U);
        return;
    }
    PushDescriptorEvent(kStatus_USB_Error, 0U);
}

static uint32_t GetInfo(usb_device_handle device, usb_host_dev_info_t code)
{
    uint32_t value = 0U;
    (void)USB_HostHelperGetPeripheralInformation(device, (uint32_t)code, &value);
    return value;
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
            usb_host_interface_t *candidate = NULL;
            configuration = (usb_host_configuration_t *)configurationHandle;
            for (interfaceIndex = 0U; interfaceIndex < configuration->interfaceCount; ++interfaceIndex)
            {
                usb_host_interface_t *interface = &configuration->interfaceList[interfaceIndex];
                if (interface->interfaceDesc->bInterfaceClass == USB_HOST_HID_CLASS_CODE)
                {
                    if (candidate == NULL)
                    {
                        candidate = interface;
                    }
                    if (interface->interfaceDesc->bInterfaceProtocol == USB_HOST_HID_PROTOCOL_MOUSE)
                    {
                        candidate = interface;
                        break;
                    }
                }
            }
            if (candidate == NULL)
            {
                return kStatus_USB_NotSupported;
            }
            s_hidDevice        = device;
            s_hidConfiguration = configurationHandle;
            s_hidInterface     = candidate;
            return kStatus_USB_Success;
        }

        case kUSB_HostEventEnumerationDone:
            if ((device == s_hidDevice) && (configurationHandle == s_hidConfiguration))
            {
                nxp_host_event_t event = {0};
                usb_host_interface_t *interface = (usb_host_interface_t *)s_hidInterface;
                usb_status_t status;
                event.kind       = NXP_HOST_EVENT_HID_ATTACHED;
                event.speed      = (uint8_t)GetInfo(device, kUSB_HostGetDeviceSpeed);
                event.address    = (uint8_t)GetInfo(device, kUSB_HostGetDeviceAddress);
                event.hubAddress = (uint8_t)GetInfo(device, kUSB_HostGetDeviceHubNumber);
                event.hubPort    = (uint8_t)GetInfo(device, kUSB_HostGetDevicePortNumber);
                event.vid        = (uint16_t)GetInfo(device, kUSB_HostGetDeviceVID);
                event.pid        = (uint16_t)GetInfo(device, kUSB_HostGetDevicePID);
                event.interfaceNumber   = interface->interfaceDesc->bInterfaceNumber;
                event.interfaceSubclass = interface->interfaceDesc->bInterfaceSubClass;
                event.interfaceProtocol = interface->interfaceDesc->bInterfaceProtocol;

                for (endpointIndex = 0U; endpointIndex < interface->epCount; ++endpointIndex)
                {
                    usb_descriptor_endpoint_t *ep = interface->epList[endpointIndex].epDesc;
                    if (((ep->bmAttributes & USB_DESCRIPTOR_ENDPOINT_ATTRIBUTE_TYPE_MASK) ==
                         USB_ENDPOINT_INTERRUPT) &&
                        ((ep->bEndpointAddress & USB_DESCRIPTOR_ENDPOINT_ADDRESS_DIRECTION_MASK) != 0U))
                    {
                        event.endpointAddress = ep->bEndpointAddress;
                        event.interval        = ep->bInterval;
                        event.maxPacketSize   = USB_SHORT_FROM_LITTLE_ENDIAN_ADDRESS(ep->wMaxPacketSize);
                        break;
                    }
                }
                PushEvent(&event);

                s_hidPacketSize = event.maxPacketSize;
                s_reportDescriptorLength = FindReportDescriptorLength(interface);
                s_reportDescriptorActualLength = 0U;
                status = USB_HostHidInit(device, &s_hidClass);
                if (status == kStatus_USB_Success)
                {
                    status = USB_HostHidSetInterface(s_hidClass,
                                                     s_hidInterface,
                                                     interface->interfaceDesc->bAlternateSetting,
                                                     HidInterfaceCallback,
                                                     NULL);
                }
                if (status != kStatus_USB_Success)
                {
                    nxp_host_event_t readyEvent = {0};
                    readyEvent.kind             = NXP_HOST_EVENT_HID_READY;
                    readyEvent.status           = (uint8_t)status;
                    PushEvent(&readyEvent);
                }
            }
            return kStatus_USB_Success;

        case kUSB_HostEventDetach:
            if (device == s_hidDevice)
            {
                nxp_host_event_t event = {0};
                usb_host_class_handle classHandle = s_hidClass;
                event.kind             = NXP_HOST_EVENT_DETACHED;
                PushEvent(&event);
                s_hidClass         = NULL;
                s_hidPacketSize    = 0U;
                s_receiveArmed     = 0U;
                s_reportDescriptorLength       = 0U;
                s_reportDescriptorActualLength = 0U;
                if (classHandle != NULL)
                {
                    (void)USB_HostHidDeinit(device, classHandle);
                }
                s_hidDevice        = NULL;
                s_hidConfiguration = NULL;
                s_hidInterface     = NULL;
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

    (void)CLOCK_EnableUsbhs1PhyPllClock(kCLOCK_Usbphy480M, 480000000U);
    (void)CLOCK_EnableUsbhs1Clock(kCLOCK_Usb480M, 480000000U);
    (void)USB_EhciPhyInit(HOST_CONTROLLER_ID, 24000000U, &phyConfig);

    return (int32_t)USB_HostInit(HOST_CONTROLLER_ID, &s_hostHandle, HostEvent);
}

void nxp_host_task(void)
{
    USB_HostEhciTaskFunction(s_hostHandle);
    if ((s_hidClass != NULL) && (s_hidPacketSize != 0U) && (s_receiveArmed == 0U))
    {
        usb_status_t status =
            USB_HostHidRecv(s_hidClass, s_hidRxBuffer, s_hidPacketSize, HidReceiveCallback, NULL);
        if (status == kStatus_USB_Success)
        {
            s_receiveArmed = 1U;
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

int32_t nxp_host_copy_report_descriptor(uint8_t *buffer, uint16_t capacity)
{
    uint16_t length = s_reportDescriptorActualLength;
    uint16_t index;
    if ((buffer == NULL) || (length == 0U) || (capacity < length))
    {
        return 0;
    }
    for (index = 0U; index < length; ++index)
    {
        buffer[index] = s_reportDescriptor[index];
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
