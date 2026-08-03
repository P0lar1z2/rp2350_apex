# Apex 键鼠到手柄转换器任务说明

## 目标

在 i.MX RT1052 Pro 上新增独立固件 `gamepad_bridge`：OTG2 Host 接收物理键盘与鼠标的 HID 输入，转换为微软标准化的 XInputHID Gamepad 报告，再由 OTG1 Device 发送给 PC。Windows 通过系统自带 `xinputhid.sys` 将它公开给 XInput 游戏；其他系统仍可按标准 HID Gamepad 使用。首版面向 PC 版 Apex 的训练场验证。

本功能是输入方式与无障碍原型，只做当前物理输入到当前手柄状态的确定性映射。不得加入压枪、自动瞄准、连点、自动身法、定时组合技、随机化规避或其他自动化逻辑；也不宣称或保证获得任何游戏辅助效果。

## 硬件与兼容性假设

- 开发板：野火 i.MX RT1052 Pro。
- OTG2：连接板载 Hub，再连接一个鼠标和一个键盘；允许两者来自不同 USB 设备。
- OTG1：连接 PC，枚举为单接口、全速、1 ms 轮询的标准 HID Gamepad；Interrupt IN 为 17 字节，Interrupt OUT 为 9 字节。
- 报告描述符逐字节采用微软 2025 年 4 月公开的 [XInputHID 规范包](https://aka.ms/gipdocs)：输入 Report ID 1，震动输出 Report ID 2。它不是已经弃用的 XUSB 协议，也不冒用 Xbox、PlayStation 等设备的 VID/PID。
- 固件中的 `cafe:1053` 是仅供开发验证的占位 VID/PID；发布产品前必须替换为合法分配的标识。
- Windows 必须包含支持该规范的系统 `xinputhid.sys`。可用 `tools/check-xinput.ps1` 直接调用 `XInputGetState` 验证；若目标系统只把它当作 DirectInput 手柄，可暂用 [Steam Input](https://partner.steamgames.com/doc/features/steam_controller/getting_started_for_players?l=english) 适配。
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

- 一个上游 1 ms 发送窗口内的相对鼠标计数先合并，再转换为右摇杆偏转；报告被 USB 端点接受后立即清空，不累计成绝对坐标。这样 8 kHz 来源不会因只保留最后一个 125 us 报告而丢失大部分移动量。
- 非零输入先跨过可配置的小死区，再按 X/Y 增益线性放大并限制到 `±32767`。
- 默认参数位于 `APEX_DEFAULT_CONFIG`；必须按实际 DPI、回报率以及游戏内手柄灵敏度实机校准。
- 收不到后续移动报告时，右摇杆最迟在 2.5 ms 后自动回中，避免粘轴。
- 滚轮是瞬时事件，因此转换为一次 30 ms 的 Y 键按下；同一保持窗口内的连续滚轮报告会合并并延长本次按下，不排队回放。

## 功能要求

1. 自动识别常见 Boot/NKRO 键盘和标准鼠标 Report Descriptor，并优先选择 Boot Keyboard/Mouse 接口。
2. 键盘和鼠标可以来自两个独立 USB 设备。
3. 输入处理、状态转换和发送路径保持 `no_std`、无堆分配、非阻塞。
4. OTG1 忙时只保留最新完整手柄状态，不排队回放过期鼠标轨迹。
5. 键盘或鼠标断开时立即释放其拥有的摇杆、按键和扳机状态；重新连接后自动恢复解析。
6. OTG1 输入报告固定为 17 字节：Report ID 1、4 个无符号 16 位摇杆轴、两个 10 位扳机、Hat、15 个按钮和 Share；内部有符号摇杆中点转换为线上 `0x8000`，Hat 中立值转换为 0。
7. 固件不复用宏引擎，确保转换路径不执行宏配置。
8. OTG1 提供 9 字节 Interrupt OUT 并接受 Report ID 2 的 XInputHID 震动报告，防止 Windows 驱动因输出路径缺失而降级；首版不驱动物理马达。

## 验收标准

### 自动化

- 根工程 `cargo test --lib --target x86_64-unknown-linux-gnu` 通过，包括移动轴、SOCD、动作键、鼠标缩放/限幅、超时回中、滚轮脉冲、Hat、释放状态和 XInputHID 线格式测试。
- 从仓库根目录执行 `cargo build --manifest-path rt1052-bringup/Cargo.toml --target thumbv7em-none-eabihf --features nxp-host,nxp-device --bin gamepad_bridge`，完成 RT1052 ELF 链接。
- 根工程 `cargo fmt --all -- --check` 通过，新增转换器与固件入口也通过独立 `rustfmt --check`；RT1052 子工程中既有文件的历史格式差异不属于本任务。

### PC 实机

1. Windows 识别 `cafe:1053 / xense / RT1052 XInputHID Gamepad`，只出现一个 HID 手柄接口、一个 Interrupt IN 和一个 Interrupt OUT。
2. `tools/check-xinput.ps1` 至少报告一个 XInput slot；操作键鼠时 `XInputGetState` 中的四轴、按钮和两个扳机发生对应变化，中立值为四轴 0、Hat/按钮 0、扳机 0。
3. WASD、鼠标、按键和滚轮逐项符合映射表；释放或拔出输入设备后不存在粘键、粘轴和粘扳机。
4. 1 kHz 鼠标输入下连续运行 30 分钟，无缓冲越界、崩溃或过期轨迹回放。
5. 在 Apex 训练场手工验证移动、视角、射击、瞄准、跳跃、蹲伏、互动、技能和菜单；任何平台兼容问题与映射/灵敏度调整均单独记录，不以自动化玩法效果作为验收指标。

## 暂不包含

- 已弃用的 XUSB 协议、GIP 主机认证、无线手柄协议。
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
- 新镜像为 88,096 字节，擦除两个 64 KiB 扇区；镜像与独立回读 SHA-256 均为 `5dfc74b8f85c920b35d3e3153785fc100c73094e4cdeef7c102b7878eaab050d`，随后目标恢复运行。
- Windows `XInputGetState` 和 Apex 训练场结果仍需在目标 PC 上完成最终确认。
