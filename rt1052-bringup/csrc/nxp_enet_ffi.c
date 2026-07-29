#include "nxp_enet_ffi.h"

#include <stdbool.h>
#include <string.h>

#include "fsl_enet.h"
#include "fsl_cache.h"

#define RX_BD_COUNT 5U
#define TX_BD_COUNT 3U
#define FRAME_BUFFER_SIZE 1536U
#define PHY_ADDRESS 0U

#define DMA_SECTION __attribute__((section(".usb_dma.enet"), aligned(64)))

static enet_handle_t s_handle;
static volatile enet_rx_bd_struct_t s_rx_bd[RX_BD_COUNT] DMA_SECTION;
static volatile enet_tx_bd_struct_t s_tx_bd[TX_BD_COUNT] DMA_SECTION;
static uint8_t s_rx_buffers[RX_BD_COUNT][FRAME_BUFFER_SIZE] DMA_SECTION;
static uint8_t s_tx_buffers[TX_BD_COUNT][FRAME_BUFFER_SIZE] DMA_SECTION;
static enet_frame_info_t s_tx_info[TX_BD_COUNT] DMA_SECTION;
static bool s_initialized;

static inline volatile uint32_t *reg32(uint32_t address) {
    return (volatile uint32_t *)(uintptr_t)address;
}

static void init_run_clock(void) {
    const clock_arm_pll_config_t arm_pll = {
        .loopDivider = 88U,
        .src = kCLOCK_PllClkSrc24M,
    };

    CLOCK_SetXtalFreq(24000000U);
    /* Move PERIPH_CLK to OSC24M while changing the ARM PLL and dividers. */
    CLOCK_SetMux(kCLOCK_PeriphClk2Mux, 1U);
    CLOCK_SetDiv(kCLOCK_PeriphClk2Div, 0U);
    CLOCK_SetMux(kCLOCK_PeriphMux, 1U);

    /* 1.25 V is the board SDK setting used before raising AHB/core clocks. */
    DCDC->REG3 = (DCDC->REG3 & ~DCDC_REG3_TRG_MASK) | DCDC_REG3_TRG(0x12U);
    while ((DCDC->REG0 & DCDC_REG0_STS_DC_OK_MASK) == 0U) {
    }

    CLOCK_InitArmPll(&arm_pll);
    CLOCK_SetDiv(kCLOCK_AhbDiv, 0U);
    CLOCK_SetDiv(kCLOCK_IpgDiv, 3U);
    CLOCK_SetDiv(kCLOCK_ArmDiv, 1U);
    CLOCK_SetMux(kCLOCK_PrePeriphMux, 3U);
    CLOCK_SetMux(kCLOCK_PeriphMux, 0U);
    SystemCoreClock = 528000000U;
}

static bool init_enet_pll(void) {
    volatile uint32_t *pll = reg32(0x400D80E0U);
    /* Match the board SDK's CLOCK_InitEnetPll({true, false, 1}) exactly. */
    *pll = (1U << 13) | 1U;
    for (uint32_t count = 0; count < 2000000U; ++count) {
        if ((*pll & (1UL << 31)) != 0U) return true;
    }
    return false;
}

static bool init_board_phy(void) {
    *reg32(0x400FC06CU) |= (3U << 26) | (3U << 10);
    *reg32(0x401F80E0U) = 5U;
    *reg32(0x401F82D0U) = 0xB0A9U;
    *reg32(0x401F80E4U) = 5U;
    *reg32(0x401F82D4U) = 0xB0A9U;
    *reg32(0x401B8004U) |= (1U << 9) | (1U << 10);
    *reg32(0x401B8000U) |= (1U << 10);
    *reg32(0x401B8000U) &= ~(1U << 9);

    *reg32(0x401F810CU) = 1U;
    *reg32(0x401F82FCU) = 0xB0E9U;
    *reg32(0x401F81B8U) = 0U;
    *reg32(0x401F8430U) = 2U;
    *reg32(0x401F83A8U) = 0xB0E9U;
    /* ALT6 + SION: drive 50 MHz to the PHY and feed the same clock into ENET. */
    *reg32(0x401F81A4U) = 6U | 0x10U;
    *reg32(0x401F842CU) = 1U;
    *reg32(0x401F8394U) = 0x31U;
    /* RMII RXD0, RXD1, CRS_DV, TXD0, TXD1, TX_EN and RX_ER. */
    *reg32(0x401F818CU) = 3U;
    *reg32(0x401F8434U) = 1U;
    *reg32(0x401F837CU) = 0xB0E9U;
    *reg32(0x401F8190U) = 3U;
    *reg32(0x401F8438U) = 1U;
    *reg32(0x401F8380U) = 0xB0E9U;
    *reg32(0x401F8194U) = 3U;
    *reg32(0x401F843CU) = 1U;
    *reg32(0x401F8384U) = 0xB0E9U;
    *reg32(0x401F8198U) = 3U;
    *reg32(0x401F8388U) = 0xB0E9U;
    *reg32(0x401F819CU) = 3U;
    *reg32(0x401F838CU) = 0xB0E9U;
    *reg32(0x401F81A0U) = 3U;
    *reg32(0x401F8390U) = 0xB0E9U;
    *reg32(0x401F81A8U) = 3U;
    *reg32(0x401F8440U) = 1U;
    *reg32(0x401F8398U) = 0xB0E9U;
    /* Select the internal ENET PLL, then enable its output on ENET_REF_CLK. */
    *reg32(0x400AC004U) = (*reg32(0x400AC004U) & ~(1U << 13)) | (1U << 17);
    if (!init_enet_pll()) {
        return false;
    }
    for (volatile uint32_t count = 0; count < 5000000U; ++count) { __asm volatile("nop"); }
    *reg32(0x401B8000U) |= (1U << 9);
    for (volatile uint32_t count = 0; count < 10000000U; ++count) { __asm volatile("nop"); }
    return true;
}

int32_t nxp_enet_init(void) {
    enet_config_t config;
    enet_buffer_config_t buffers;
    uint8_t mac[6] = {0x02, 0x10, 0x52, 0x00, 0x00, 0x01};
    init_run_clock();
    /* Buffer descriptors are shared with DMA and must never remain in D-cache. */
    L1CACHE_DisableDCache();
    if (!init_board_phy()) {
        return -1;
    }
    memset(&buffers, 0, sizeof(buffers));
    buffers.rxBdNumber = RX_BD_COUNT;
    buffers.txBdNumber = TX_BD_COUNT;
    buffers.rxBuffSizeAlign = FRAME_BUFFER_SIZE;
    buffers.txBuffSizeAlign = FRAME_BUFFER_SIZE;
    buffers.rxBdStartAddrAlign = s_rx_bd;
    buffers.txBdStartAddrAlign = s_tx_bd;
    buffers.rxBufferAlign = &s_rx_buffers[0][0];
    buffers.txBufferAlign = &s_tx_buffers[0][0];
    buffers.rxMaintainEnable = false;
    buffers.txMaintainEnable = false;
    buffers.txFrameInfo = s_tx_info;

    ENET_GetDefaultConfig(&config);
    config.interrupt = 0U;
    if (ENET_Init(ENET, &s_handle, &config, &buffers, mac, 132000000U) != kStatus_Success) {
        return -2;
    }
    /* Advertise 10/100 Mbit/s, half/full duplex, then restart negotiation. */
    if (ENET_MDIOWrite(ENET, PHY_ADDRESS, 4U, 0x01E1U) != kStatus_Success ||
        ENET_MDIOWrite(ENET, PHY_ADDRESS, 0U, 0x1200U) != kStatus_Success) {
        return -3;
    }
    ENET_SetMII(ENET, kENET_MiiSpeed100M, kENET_MiiFullDuplex);
    ENET_ActiveRead(ENET);
    s_initialized = true;
    return 0;
}

int32_t nxp_enet_status(nxp_enet_status_t *status) {
    uint16_t value;
    if (!s_initialized || status == NULL) return -1;
    memset(status, 0, sizeof(*status));
    if (ENET_MDIORead(ENET, PHY_ADDRESS, 2U, &status->phy_id1) != kStatus_Success ||
        ENET_MDIORead(ENET, PHY_ADDRESS, 3U, &status->phy_id2) != kStatus_Success ||
        ENET_MDIORead(ENET, PHY_ADDRESS, 1U, &value) != kStatus_Success ||
        ENET_MDIORead(ENET, PHY_ADDRESS, 1U, &status->bmsr) != kStatus_Success ||
        ENET_MDIORead(ENET, PHY_ADDRESS, 31U, &status->scsr) != kStatus_Success) return -2;
    status->link_up = (status->bmsr & 0x0004U) != 0U;
    uint16_t mode = (status->scsr >> 2) & 7U;
    status->speed_100m = (mode == 2U || mode == 6U) ? 1U : 0U;
    status->full_duplex = (mode == 5U || mode == 6U) ? 1U : 0U;
    if (status->link_up) {
        ENET_SetMII(ENET, status->speed_100m ? kENET_MiiSpeed100M : kENET_MiiSpeed10M,
                    status->full_duplex ? kENET_MiiFullDuplex : kENET_MiiHalfDuplex);
    }
    return 0;
}

int32_t nxp_enet_receive(uint8_t *frame, uint32_t capacity) {
    uint32_t length = 0;
    if (!s_initialized || frame == NULL) return -1;
    status_t result = ENET_GetRxFrameSize(&s_handle, &length, 0U);
    if (result == kStatus_ENET_RxFrameEmpty) return 0;
    if (result != kStatus_Success || length > capacity) {
        (void)ENET_ReadFrame(ENET, &s_handle, NULL, 0U, 0U, NULL);
        return -2;
    }
    if (ENET_ReadFrame(ENET, &s_handle, frame, length, 0U, NULL) != kStatus_Success) return -3;
    return (int32_t)length;
}

int32_t nxp_enet_send(const uint8_t *frame, uint32_t length) {
    if (!s_initialized || frame == NULL || length < 14U || length > FRAME_BUFFER_SIZE) return -1;
    status_t result = ENET_SendFrame(ENET, &s_handle, frame, length, 0U, false, NULL);
    return result == kStatus_Success ? 0 : -2;
}

int32_t nxp_enet_set_phy_loopback(uint8_t enable) {
    if (!s_initialized) return -1;
    /* Exercise the 100 Mbit/s full-duplex RMII data path. */
    uint16_t bmcr = enable != 0U ? 0x6100U : 0x1200U;
    if (enable != 0U) {
        ENET_SetMII(ENET, kENET_MiiSpeed100M, kENET_MiiFullDuplex);
    }
    return ENET_MDIOWrite(ENET, PHY_ADDRESS, 0U, bmcr) == kStatus_Success ? 0 : -2;
}

uint32_t nxp_enet_cpu_hz(void) {
    return CLOCK_GetFreq(kCLOCK_CpuClk);
}
