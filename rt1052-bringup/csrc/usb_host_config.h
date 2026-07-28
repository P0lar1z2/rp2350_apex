#ifndef RT1052_USB_HOST_CONFIG_H
#define RT1052_USB_HOST_CONFIG_H

#define USB_HOST_CONFIG_KHCI                         (0U)
#define USB_HOST_CONFIG_EHCI                         (1U)
#define USB_HOST_CONFIG_OHCI                         (0U)
#define USB_HOST_CONFIG_IP3516HS                     (0U)
#define USB_HOST_CONFIG_MAX_HOST                     (1U)
#define USB_HOST_CONFIG_MAX_PIPES                    (16U)
#define USB_HOST_CONFIG_MAX_TRANSFERS                (32U)
#define USB_HOST_CONFIG_INTERFACE_MAX_EP             (8U)
#define USB_HOST_CONFIG_CONFIGURATION_MAX_INTERFACE (8U)
#define USB_HOST_CONFIG_MAX_POWER                    (250U)
#define USB_HOST_CONFIG_ENUMERATION_MAX_RETRIES      (3U)
#define USB_HOST_CONFIG_ENUMERATION_MAX_STALL_RETRIES (1U)
#define USB_HOST_CONFIG_MAX_NAK                      (3000U)
#define USB_HOST_CONFIG_BUFFER_PROPERTY_CACHEABLE    (0U)
#define USB_HOST_CONFIG_COMPLIANCE_TEST              (0U)
#define USB_HOST_CONFIG_CLASS_AUTO_CLEAR_STALL       (1U)
#define USB_HOST_CONFIG_USE_TASK                     (0U)

#define USB_HOST_CONFIG_EHCI_FRAME_LIST_SIZE (1024U)
#define USB_HOST_CONFIG_EHCI_MAX_QH          (16U)
#define USB_HOST_CONFIG_EHCI_MAX_QTD         (32U)
#define USB_HOST_CONFIG_EHCI_MAX_ITD         (0U)
#define USB_HOST_CONFIG_EHCI_MAX_SITD        (0U)

#define USB_HOST_CONFIG_HUB     (1U)
#define USB_HOST_CONFIG_HID     (1U)
#define USB_HOST_CONFIG_MSD     (0U)
#define USB_HOST_CONFIG_CDC     (0U)
#define USB_HOST_CONFIG_CDC_ECM (0U)
#define USB_HOST_CONFIG_CDC_RNDIS (0U)
#define USB_HOST_CONFIG_AUDIO   (0U)
#define USB_HOST_CONFIG_PHDC    (0U)
#define USB_HOST_CONFIG_PRINTER (0U)

#endif
