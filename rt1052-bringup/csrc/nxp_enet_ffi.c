#include "nxp_enet_ffi.h"

#include <stdbool.h>
#include <string.h>

#include "fsl_enet.h"

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

static bool init_enet_pll(void) {
    volatile uint32_t *pll = reg32(0x400D80E0U);
    *pll |= (1U << 16);
    *pll = (*pll & ~((3U << 0) | (1U << 12))) | 1U | (1U << 13);
    for (uint32_t count = 0; count < 2000000U; ++count) {
        if ((*pll & (1UL << 31)) != 0U) {
            *pll &= ~(1U << 16);
            return true;
        }
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
    *reg32(0x401F81A4U) = 6U;
    *reg32(0x401F842CU) = 1U;
    *reg32(0x401F8394U) = 0x31U;
    *reg32(0x400AC004U) |= (1U << 17);
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
    buffers.txFrameInfo = s_tx_info;

    ENET_GetDefaultConfig(&config);
    config.interrupt = 0U;
    if (ENET_Init(ENET, &s_handle, &config, &buffers, mac, 132000000U) != kStatus_Success) {
        return -2;
    }
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
