/*
 * RAM-only USB bring-up layout for MIMXRT1052CVL5B. These are the default
 * regions from NXP's MIMXRT1052xxxxx_ram.ld. cortex-m-rt calls the executable
 * region FLASH, but it is the chip's on-chip ITCM.
 */
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 128K
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K
  OCRAM : ORIGIN = 0x20200000, LENGTH = 256K
}

SECTIONS
{
  /* imxrt-usbd's endpoint state contains constructor state and must be
   * initialized by the RAM loader, unlike the NXP Host's NOLOAD DMA pool. */
  .usb_device : ALIGN(4096)
  {
    __usb_device_start = .;
    KEEP(*(.usb_device .usb_device.*));
    . = ALIGN(32);
    __usb_device_end = .;
  } > OCRAM

  .usb_dma (NOLOAD) : ALIGN(32)
  {
    __usb_dma_start = .;
    KEEP(*(.usb_dma .usb_dma.*));
    . = ALIGN(32);
    __usb_dma_end = .;
  } > OCRAM
} INSERT AFTER .uninit;
