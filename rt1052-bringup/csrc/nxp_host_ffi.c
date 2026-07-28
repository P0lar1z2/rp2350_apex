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
#define REPORT_DATA_CAPACITY (16U)
#define HID_RX_BUFFER_SIZE (64U)

static usb_host_handle s_hostHandle;
static usb_device_handle s_hidDevice;
static usb_host_configuration_handle s_hidConfiguration;
static usb_host_interface_handle s_hidInterface;
static usb_host_class_handle s_hidClass;
static uint16_t s_hidPacketSize;
__attribute__((section(".usb_dma.hid_rx"), aligned(32)))
static uint8_t s_hidRxBuffer[HID_RX_BUFFER_SIZE];
static nxp_host_event_t s_events[EVENT_QUEUE_CAPACITY];
static volatile uint8_t s_eventRead;
static volatile uint8_t s_eventWrite;
static nxp_host_report_t s_reports[REPORT_QUEUE_CAPACITY];
static volatile uint8_t s_reportRead;
static volatile uint8_t s_reportWrite;
static uint32_t s_reportSequence;

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

static void HidReceiveCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    nxp_host_report_t report = {0};
    uint32_t index;
    uint32_t length = dataLen;
    (void)param;

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

    if (s_hidClass != NULL)
    {
        (void)USB_HostHidRecv(s_hidClass, s_hidRxBuffer, s_hidPacketSize, HidReceiveCallback, NULL);
    }
}

static void HidInterfaceCallback(void *param, uint8_t *data, uint32_t dataLen, usb_status_t status)
{
    nxp_host_event_t event = {0};
    (void)param;
    (void)data;
    (void)dataLen;

    event.kind   = NXP_HOST_EVENT_HID_READY;
    event.status = (uint8_t)status;
    if ((status == kStatus_USB_Success) && (s_hidClass != NULL) && (s_hidPacketSize != 0U))
    {
        status = USB_HostHidRecv(s_hidClass, s_hidRxBuffer, s_hidPacketSize, HidReceiveCallback, NULL);
        event.status = (uint8_t)status;
    }
    PushEvent(&event);
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
                if (s_hidPacketSize > HID_RX_BUFFER_SIZE)
                {
                    s_hidPacketSize = HID_RX_BUFFER_SIZE;
                }
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
