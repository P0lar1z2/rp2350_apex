#ifndef RT1052_NXP_ENET_FFI_H
#define RT1052_NXP_ENET_FFI_H

#include <stdint.h>

typedef struct nxp_enet_status {
    uint16_t phy_id1;
    uint16_t phy_id2;
    uint16_t bmsr;
    uint16_t scsr;
    uint8_t link_up;
    uint8_t speed_100m;
    uint8_t full_duplex;
    uint8_t reserved;
} nxp_enet_status_t;

int32_t nxp_enet_init(void);
int32_t nxp_enet_status(nxp_enet_status_t *status);
int32_t nxp_enet_receive(uint8_t *frame, uint32_t capacity);
int32_t nxp_enet_send(const uint8_t *frame, uint32_t length);

#endif
