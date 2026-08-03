# Apex 键鼠到手柄转换器任务说明

## 目标

在 i.MX RT1052 Pro 上新增独立固件 `gamepad_bridge`：OTG2 Host 接收物理键盘与鼠标的 HID 输入，转换为手柄状态，再由 OTG1 Device 发送给 PC。最终目标仍是使用微软标准化的 XInputHID 报告与合法分配的设备标识；当前烧写候选先临时切换到 Xbox 360 有线 XUSB 模式，以单独验证 Windows、XInput 和 Apex 的完整输入链路。

本功能是输入方式与无障碍原型，只做当前物理输入到当前手柄状态的确定性映射。不得加入压枪、自动瞄准、连点、自动身法、定时组合技、随机化规避或其他自动化逻辑；也不宣称或保证获得任何游戏辅助效果。

## 硬件与兼容性假设

- 开发板：野火 i.MX RT1052 Pro。
- OTG2：连接板载 Hub，再连接一个鼠标和一个键盘；允许两者来自不同 USB 设备。
- OTG1：连接 PC。标准方案枚举为单接口 XInputHID Gamepad；当前临时验证方案枚举为 Xbox 360 有线设备的四个厂商接口，游戏报告端点为 32 字节、实际输入包为 20 字节、输出命令为 8 字节。
- 标准方案的报告描述符逐字节采用微软 2025 年 4 月公开的 [XInputHID 规范包](https://aka.ms/gipdocs)：输入 Report ID 1，震动输出 Report ID 2；对应实现仍保留在源码中。
- 当前验证镜像临时使用 `045e:028e`、设备级 `ff/ff/ff` 和接口 `ff/5d/01`，让 Windows 10 直接加载 Xbox 360 系统驱动。该 Microsoft VID/PID 仅用于本机原型排障，禁止作为产品标识或对外分发；发布前必须恢复合法分配的 VID/PID 与正式驱动绑定方案。
- XUSB 四接口布局和 20 字节报告格式参考已在 Windows PC 上验证的 MIT 许可 [GP2040-CE XInput 实现](https://github.com/OpenStickCommunity/GP2040-CE/tree/main/src/drivers/xinput)。当前目标仅为 Windows PC，未实现也不尝试绕过 Xbox 主机认证。
- 可用 `tools/check-xinput.ps1` 直接调用 `XInputGetState` 验证；若标准 XInputHID 方案只被识别为 DirectInput 手柄，可暂用 [Steam Input](https://partner.steamgames.com/doc/features/steam_controller/getting_started_for_players?l=english) 适配。
- 主机平台、认证手柄透传、物理震动马达和灯光不在首版范围内；固件只接收并丢弃规范要求的震动输出报告。

## 默认 Apex 映射

| 键鼠输入 | HID 手柄输出 | 游戏语义 |
|---|---|---|
| W / A / S / D | 左摇杆 | 移动 |
| 鼠标 X / Y | 右摇杆 | 视角 |
| 鼠标左键 | RT | 射击 |
| 鼠标右键 | LT | 瞄准 |
| Space | A / South | 跳跃 |
| Ctrl 或 C | B / East | 蹲伏 |
| E 或 R | X / West | 互动 / 换弹 |
| 1、2、3 或滚轮 | Y / North | 切换武器；按住 3 对应收起武器 |
| Q 或鼠标侧键 4 | LB | 战术技能 |
| 鼠标中键 | RB | 标记 |
| Z | LB + RB | 终极技能的同语义组合输入 |
| Shift | 左摇杆按下 | 冲刺 |
| V 或鼠标侧键 5 | 右摇杆按下 | 近战 |
| M | View | 地图 |
| Tab 或 Esc | Menu | 背包 / 菜单 |
| 4 / G / H / N(B) | 十字键上 / 右 / 下 / 左 | 治疗 / 手雷 / 额外角色动作 / 检视（射击模式） |

以上语义以 EA 当前的 [Apex PC 与 Xbox 默认控制表](https://help.ea.com/en/articles/apex-legends/pc-and-controller-settings/) 为基准；玩家若改过游戏内布局，应同步调整固件映射。

左右或前后同时按下时，对应移动轴回中；斜向移动归一化到约 70.7%，避免数字键盘输入产生超出单位圆的幅值。

## 鼠标到右摇杆算法

- 一个上游 1 ms 发送窗口内的相对鼠标计数先合并，再转换为右摇杆偏转；报告被 USB 端点接受后只消费本批相对计数，生成的绝对摇杆采样会保持到下一批移动更新或超时回中，不累计成绝对坐标。这样既不会丢失同一窗口中的高回报率报告，也避免 XInput/Apex 按帧读取时只看到紧随其后的回中包。
- 非零输入先施加可配置的反死区，再按 X/Y 增益线性放大并限制到 `±32767`。当前反死区为 `10000`，高于 XInput.h 建议的右摇杆死区 `8689`；此前的 `2048` 虽能在 `joyctl` 中看到，但可能被 Apex 完全过滤。
- 默认参数位于 `APEX_DEFAULT_CONFIG`；必须按实际 DPI、回报率以及游戏内手柄灵敏度实机校准。
- 收不到后续移动报告时，右摇杆在 20 ms 后自动回中；这个窗口覆盖一次 60 Hz 状态采样，同时限制停止鼠标后的尾部转动。
- 滚轮是瞬时事件，因此转换为一次 30 ms 的 Y 键按下；同一保持窗口内的连续滚轮报告会合并并延长本次按下，不排队回放。

## 功能要求

1. 自动识别常见 Boot/NKRO 键盘和标准鼠标 Report Descriptor，并优先选择 Boot Keyboard/Mouse 接口。
2. 键盘和鼠标可以来自两个独立 USB 设备。
3. 输入处理、状态转换和发送路径保持 `no_std`、无堆分配、非阻塞。
4. OTG1 忙时只保留最新完整手柄状态，不排队回放过期鼠标轨迹。
5. 键盘或鼠标断开时立即释放其拥有的摇杆、按键和扳机状态；重新连接后自动恢复解析。
6. 转换器同时保留两种编码：XInputHID 为固定 17 字节；临时 XUSB 为固定 20 字节，包含两个按键字节、两个 8 位扳机、四个有符号 16 位摇杆轴和 6 字节保留区。XUSB 的 Y 轴在编码时转换为 XInput 的“向上为正”。
7. 固件不复用宏引擎，确保转换路径不执行宏配置。
8. XInputHID 模式提供 9 字节 Interrupt OUT；临时 XUSB 模式接受 8 字节震动/LED 输出命令。首版只接收并丢弃，不驱动物理马达或灯光。

## 验收标准

### 自动化

- 根工程 `cargo test --lib --target x86_64-unknown-linux-gnu` 通过，包括移动轴、SOCD、动作键、鼠标缩放/限幅、超时回中、滚轮脉冲、Hat、释放状态和 XInputHID 线格式测试。
- 从仓库根目录执行 `cargo build --manifest-path rt1052-bringup/Cargo.toml --target thumbv7em-none-eabihf --features nxp-host,nxp-device --bin gamepad_bridge`，完成 RT1052 ELF 链接。
- 根工程 `cargo fmt --all -- --check` 通过，新增转换器与固件入口也通过独立 `rustfmt --check`；RT1052 子工程中既有文件的历史格式差异不属于本任务。

### PC 实机

1. 当前临时验证镜像在 Windows 识别为 `045e:028e / Xbox 360 Controller for Windows`，由系统 Xbox/XUSB 驱动接管，不再显示为通用 `HidUsb` 游戏手柄；恢复标准方案后再按 `cafe:1053` 的单 HID 接口重新验收。
2. `tools/check-xinput.ps1` 至少报告一个 XInput slot；操作键鼠时 `XInputGetState` 中的四轴、按钮和两个扳机发生对应变化，中立值为四轴 0、Hat/按钮 0、扳机 0。
3. WASD、鼠标、按键和滚轮逐项符合映射表；释放或拔出输入设备后不存在粘键、粘轴和粘扳机。
4. 1 kHz 鼠标输入下连续运行 30 分钟，无缓冲越界、崩溃或过期轨迹回放。
5. 在 Apex 训练场手工验证移动、视角、射击、瞄准、跳跃、蹲伏、互动、技能和菜单；任何平台兼容问题与映射/灵敏度调整均单独记录，不以自动化玩法效果作为验收指标。

## 暂不包含

- XUSB 作为最终发布协议、GIP/Xbox 主机认证、无线手柄协议；XUSB 仅作为当前 Windows PC 链路验证手段。
- 物理 Force Feedback / rumble、LED、电量和音频端点。
- 宏、Turbo、压枪曲线、武器识别、屏幕识别或网络控制自动化。
- 游戏反作弊绕过、设备指纹伪装或隐藏物理输入来源。

## 2026-08-02 实机记录

- RAM 运行与 FlexSPI 启动均在 Linux 上枚举为 `cafe:1052 xense RT1052 KBM Gamepad`，Full-Speed、单 HID 接口、13 字节 Interrupt IN、`bInterval=1`。
- Linux `usbhid` 成功绑定，并创建 `event2` 与 `js0`；输入子系统标记为 joystick。
- 烧写前生成新的 32 MiB W25Q256 备份，SHA-256 为 `59000e4e3daad2fb5401e8727c034468763bcab82feb1651246f8f2b8833579f`。
- 烧写镜像为 84,000 字节，擦除两个 64 KiB 扇区；镜像和独立回读 SHA-256 均为 `d007a929f3ceeb2e8b2758502d77f8ff2d78f34e48efcd5af52957cf55d233b0`。
- 本次 OTG2 枚举中 Razer `1532:00b8` 的接口 0/1 可用；Dell `413c:2113` 的接口接收器/描述符初始化失败，因此转换器退选 Razer 接口 1 作为键盘来源。需要手动验证实际 WASD 来源，并继续修复独立键盘枚举稳定性。

## 2026-08-03 Windows 兼容修正

- 旧 `cafe:1052` 通用 HID 版本在 Windows `joy.cpl` 中枚举正常，属性页也能观测键鼠转换后的轴和按钮，但 Apex 完全不读取。这证明 USB、OTG2 输入和转换逻辑正常，缺失的是游戏使用的 XInput 接入路径。
- 固件改为微软公开的 XInputHID 报告格式，新增 9 字节 Interrupt OUT，将开发 PID 改为 `cafe:1053`，产品名改为 `RT1052 XInputHID Gamepad`，避免 Windows 复用旧设备节点。
- 主机单元测试 47 项通过；RT1052 RAM release 和 FlexSPI XIP release 均成功链接。
- 烧写前完整读取 32 MiB W25Q256，SHA-256 为 `530d9a9f5b5f9f519c6afbc84d6c2d95a38d88b166f1eab938bc62e9b6093e67`。
- 第一版 XInputHID 镜像的 283 字节 Report Descriptor 超过 `usb-device` 的 256 字节复制式 EP0 缓冲；请求未完整返回，Windows 10 将 `USB\VID_CAFE&PID_1053` 标记为 Code 10 / `CM_PROB_FAILED_START`。
- `RuntimeHid` 改为持有固件静态描述符，并通过 `accept_with_static` 在 EP0 上直接流式发送完整 283 字节。修正镜像仍为 88,096 字节，擦除两个 64 KiB 扇区；镜像与独立回读 SHA-256 均为 `e8d8cbb82c1e821abde39ce6a80c8805ab99886e51a415ac314f06fceaeaa0e5`，随后目标恢复运行。
- 修正后 Windows 设备状态恢复为 `CM_PROB_NONE`，并成功创建 `HID-compliant game controller` 子设备；但仍由通用 `HidUsb` 接管，`XInputGetState` 的 0–3 槽位均为 `NO_XINPUT_CONTROLLER`。这证明剩余问题是 `xinputhid` 驱动绑定，而不是 HID 描述符传输。
- 为先验证 XInput/Apex 链路，新增独立 `RuntimeXinput`，当前构建临时模拟 Xbox 360 有线控制器：四个 XUSB 接口、固定端点地址和 20 字节输入包。49 项主机单元测试与 RT1052 RAM/XIP 目标链接均通过。
- 临时 XUSB 镜像为 88,096 字节，复用备份 SHA-256 `530d9a9f5b5f9f519c6afbc84d6c2d95a38d88b166f1eab938bc62e9b6093e67` 的板级启动头；擦除两个 64 KiB 扇区并写入 88,320 字节后，镜像与独立回读 SHA-256 均为 `614b8ac150f8fd46640543f120d72a57b74adaabae5af2bee0779853f45155c3`，目标随后恢复运行。Windows 枚举和 XInput 槽位仍待确认。
- 首次 XUSB 实测发现鼠标生成的右摇杆状态在 USB IN 接受后立即回中，XInput/Apex 的按帧采样可能只看到中立状态。修正后每批相对计数仍只消费一次，但生成的绝对右摇杆状态会保持到下一批移动或 20 ms 超时；新批次替换旧状态，不形成绝对坐标累积。修正版镜像与独立回读 SHA-256 均为 `202eedd43c7734f7deae37807a227145c7f6640a14237da07cd12db44165590f`，烧写后目标恢复运行。
- Windows `joyctl` 随后能看到鼠标对应的 `RX/RY`，且 Apex 能正常读取同一 XInput 设备的 WASD/左摇杆，但游戏视角仍不动，进一步定位为右摇杆幅值被游戏死区过滤。默认鼠标反死区由 `2048` 提高到 `10000`，略高于 XInput.h 的建议右摇杆死区 `8689`；镜像与独立回读 SHA-256 均为 `6ef1a6dc594598bf23d8bed8030b6e2483cb6ac8f8d9a6253facf23f6773d9e6`，目标已恢复运行。待实机确认视角开始移动后再校准增益与响应曲线。
