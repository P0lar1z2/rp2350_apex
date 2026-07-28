# RP2350 到 i.MX RT1052 迁移任务

## 当前硬件

- 开发板：野火 i.MX RT1052 Pro
- MCU：MIMXRT1052CVL5B
- 调试器：DAP，5 针 SWD 已连接
- USB Host：OTG2 连接板载 FE1.1S Hub，再连接鼠标
- USB Device：OTG1 连接上位机
- 以太网：后续作为低优先级控制面
- DAP `reset-halt` 后采用 NXP 默认 FlexRAM：128 KiB ITCM + 128 KiB DTCM + 256 KiB OCRAM

## 技术方案

- 主工程和业务逻辑继续使用 Rust `no_std`。
- OTG2 Host 使用 NXP MCUXpresso SDK 26.06 LTS 的 EHCI、Host Core、Hub 和 HID Class，通过薄 C FFI 接入 Rust。
- OTG1 Device 优先继续使用 Rust `imxrt-usbd`/`usb-device`，保留动态 HID 克隆能力。
- 不使用 Keil；NXP C 代码用 GNU Arm 工具链编译并静态链接到 Rust 固件。
- 不引入 FreeRTOS；先采用裸机事件循环。
- 配套资料及完整 NXP SDK 不提交 Git；工程只记录版本、获取方式和必要的本地适配代码。

## 实施阶段

### 1. USB Host 探针

- [x] 使用 NXP 默认 128 KiB ITCM + 128 KiB DTCM + 256 KiB OCRAM；不做运行期重分配。
- [x] 为 USB DMA 建立 32 字节对齐、非缓存的 OCRAM 区域。
- [x] 接入 NXP 26.06 LTS 的 EHCI Host 最小源文件集。
- [x] 完成并实机验收 OTG2/USBPHY2 初始化和 `USB_OTG2` 中断代码。
- [x] 枚举板载 FE1.1S Hub（地址 1）。
- [x] 枚举 Hub 端口 3 下的鼠标（17ef:62c2，地址 2）。
- [x] 输出鼠标 speed、VID、PID、端点地址、`bInterval`、`wMaxPacketSize`。
- [x] 连续读取并输出 HID Interrupt IN 报告（实测收到 7 字节原始鼠标报告）。
- [ ] 读取并输出 HID Report Descriptor。

### 2. 8K Host 接收

- [ ] 只接受实际以 High-Speed 枚举且 Interrupt IN `bInterval=1` 的 8K 路径。
- [ ] 预挂至少 8 个独立接收缓冲和 qTD，避免主循环重挂产生空窗。
- [ ] C 回调仅写入有界无锁队列，不解析 HID、不打印日志。
- [ ] Rust 消费原始报告并记录报告数、间隔、丢包和队列高水位。
- [ ] 连续 10 秒接收约 80,000 个有效轮询周期，并检查 125 us 调度抖动。

### 3. USB Device 与桥接

- [ ] OTG1 以 High-Speed Device 枚举到上位机。
- [ ] 根据来源设备描述符创建 HID 接口和 Interrupt IN 端点。
- [ ] 8K 来源设备保持 `bInterval=1`，不得在桥接层降为 1K。
- [ ] 迁移现有 HID 描述符解析、动态克隆和宏引擎。
- [ ] 处理热插拔、STALL、超时和设备重新枚举。

### 4. Ethernet 控制

- [ ] 接入 ENET DMA 和网络栈。
- [ ] USB Host 中断优先级高于 USB Device，高于 ENET。
- [ ] 网络协议只发送控制事件，不进入 USB 中断路径。
- [ ] 在满载网络流量下复测 8K USB，不允许出现持续丢报告或降频。

## 首轮验收

1. DAP 可稳定下载和复位固件。
2. OTG2 首先识别 FE1.1S，再识别已连接鼠标。
3. 日志能准确给出鼠标速度和 Interrupt IN 端点参数。
4. 若鼠标为 High-Speed、`bInterval=1`，进入 8K 多缓冲测试；否则明确报告实际速率，不伪造 8K。
5. OTG1 和以太网在 Host 探针通过前不参与数据转发。

## 当前工作

- 分支：`feature/rt1052-nxp-usb-host`
- 已完成：NXP USB Host 2.12.2 构建、裸机 OSA、Rust FFI、OTG2 枚举探针。
- 已验证：NXP 默认 OCRAM `0x20200000` 的 CPU 写回与 DAP 回读通过；OTG2 EHCI、Hub、HID 枚举通过。
- 实测鼠标：接口 1、协议 2（Mouse）、Full-Speed，Interrupt IN `0x82`，`wMaxPacketSize=8`，`bInterval=1`，因此当前链路最多 1 kHz，不是 8 kHz。
- 实测接收：15 秒窗口内移动鼠标得到 10 个成功的 7 字节原始报告；NXP 回调重挂、DTCM 有界队列和 Rust 消费链路通过。
- 当前阻塞：无。
- 下一步：读取 HID Report Descriptor，接入现有 Rust 描述符解析与动态克隆逻辑。
