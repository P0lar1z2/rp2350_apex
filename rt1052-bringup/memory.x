/*
 * Default RAM bring-up layout for MIMXRT1052CVL5B. With the flash-xip feature,
 * build.rs defines __flash_xip and the executable region moves behind the
 * RT1052 ROM's 8 KiB FCB/IVT header in the external 32 MiB FlexSPI NOR.
 */
MEMORY
{
  FLASH : ORIGIN = DEFINED(__flash_xip) ? 0x60002000 : 0x00000000,
          LENGTH = DEFINED(__flash_xip) ? 0x01FFE000 : 128K
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
  } > OCRAM AT > FLASH
  __usb_device_load = LOADADDR(.usb_device);

  .usb_dma (NOLOAD) : ALIGN(32)
  {
    __usb_dma_start = .;
    KEEP(*(.usb_dma .usb_dma.*));
    . = ALIGN(32);
    __usb_dma_end = .;
  } > OCRAM
} INSERT AFTER .uninit;
