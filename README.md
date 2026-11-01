# Tetris99Clone — Tetris 99 复刻（Arena Module for AX1）

[English](README.en.md) | 简体中文

确定性 Tetris 99 风格对局引擎 + 无头评测主机 + Bevy 人类客户端 + 内置 Bot 梯队。
同一套物理服务**人类游玩端**与**程序游玩端**：人类客户端只是输入事件的另一种来源，与子进程程序、内置 Bot 走完全相同的校验与物理路径。

## 声明

- 本项目几乎完全由生成式人工智能技术（Generative AI）/ 大语言模型（Large Language Model）完成，我们不对项目的可用性和规范性负责。本项目按"原样"提供，带着"可能会有用"的期许，但**不做任何保证**。
- 选择 Tetris 99 游戏机制进行复刻，是为了最大化模型与人类的熟悉程度。本项目**无意也并未**完整复刻 Tetris 99 的游戏功能与机制；在不妨碍模型解题的基础上，我们对原版规则进行了变更。
- 本项目仅使用了 Tetris Guideline 与 Tetris 99 的解读文本来完成，**未对 Tetris 99 游戏程序进行反编译与逆向处理**。
- Tetris 99 的游戏版权由 **Nintendo 株式会社**所有。

## 组件

```
t99-core      确定性内核：整数物理/SRS/7-bag/垃圾/目标/徽章/名次（零外部依赖）
t99-proto     线协议 v8（单行 JSON）+ 适配器（stdio/WS）+ snapshot_from_frame 反向构造
t99-bots      内置 Bot（按档分文件）：null / basic / advanced / expert（进程内、同视角、
              同限速；basic/advanced 源码公开，expert 为闭源内建、挂 feature `expert`）
t99-bot-ext   公开档外部程序：t99-bot-basic / t99-bot-advanced（参考实现/可修改陪练，
              决策核与 internal 同一份代码；随 dist 分发）
t99-bot-m     隐藏档外部程序 Master/GrandMaster（Arena 公开包剔除本目录；参赛方仅知其存在）
t99-config    ruleset.toml / server.toml / client.toml 解析（CLI > env > file > 默认值）
t99-headless  无头主机：run（对局，支持 ext:label=path 外部席位）/ replay（逐位回放校验）/
              calibrate（梯队矩阵）
t99-server    单局权威服务器：ID+Key 准入、事件流广播、观战席位、大厅模式
t99-client    Bevy 0.16 人类客户端（本地 1 人 + 3 Bot，含 HUD/落点影/垃圾表；不随 dist 分发）
examples/     pybot.py：Python 子进程示例程序（跨语言协议验证）
utils/        time.py：Arena Utils 会话墙钟查询
Makefile      make check（测试+冒烟）/ make dist（musl 静态分发 → dist/）
```

正式参赛阵容：basic/advanced/expert 的 **internal** 版 + master/grandmaster 的 **external** 版；
external 的 basic/advanced 是参考实现与陪练，与 internal 构造上等价但不做 1:1 断言。

## 快速开始

```bash
cargo build --release
make check                                # 全 workspace 测试 + ext 冒烟
make dist                                 # musl 静态分发 → dist/（含剔除断言与自洽构建检查）

# 4 人 Bot 对局（basic/advanced/expert/advanced），15 分钟上限，7 秒跑完
./target/release/t99-headless run --players 4 --bots basic,advanced,expert,advanced \
    --seed 31415 --transcript /tmp/m.jsonl

# 逐位回放校验（种子+事件流 → 同 hash）
./target/release/t99-headless replay --transcript /tmp/m.jsonl

# 梯队校准矩阵（每对 N 局，验证档位严格传递）
./target/release/t99-headless calibrate --tiers null,basic,advanced,expert --games 12

# 外部程序席位（ext:label=path，与参赛模型提交同通道）
./target/release/t99-headless run --players 4 --seed 7 \
    --bots "ext:basic=./target/release/t99-bot-basic,advanced,expert,basic"

# C/S（含 B/S）：服务器 = 一局对局；客户端本地镜像重演事件流（确定性，hash 校验一致）
# 对局倒计时按人数自动：4p=5min → 99p=15min（--time-limit-secs 覆盖）；
# --stall-accel 1/0：0 杀 60s 后倒计时按 T/60 倍速燃烧（展览机制，正式 Arena 关闭）
./target/release/t99-server --port 8138 --key t99 --players 4 --lobby \
    --bots "ext:basic=./target/release/t99-bot-basic,advanced,expert,basic"
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id p1 --key t99          # 玩家
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id ob --key t99 --observer --spectate 0  # 观战

# 浏览器版（wasm，WebGL2）
./scripts-build-web.sh && cd crates/t99-client/web && python3 -m http.server 8137

# 子进程程序参赛（示例：Python 贪心 vs 内置 basic）
./target/release/t99-headless run --players 2 --bots basic \
    --candidate "python3 examples/pybot.py" --seed 99
```

客户端按键：`A/D` 移动（按住连发，引擎侧 DAS/ARR）· `S` 软降 · `空格` 硬降 · `X/J` 顺时针 ·
`Z/K` 逆时针 · `C/Shift` Hold · `Tab` 循环目标模式 · **方向键导航小窗手动锁敌**。

## 协议要点（v8）

- 单行 JSON：`hello → ready → (frame → input)* → result`；`ready` 限时 2s，帧响应预算
  16.666ms（判定阈值 16ms），连续 600 帧无有效响应判负，进程非零退出判负。
- **压放式按键**：`left_down/left_up/...`，DAS/ARR 由引擎统一执行（所有参与方同物理）；
  事件偏移用整数微秒 `t_us ∈ [0,16666]`，每帧 ≤16 事件、严格递增，非法整帧作废。
- 五种目标模式（badges/kos/attackers/random/manual）；快照含 `phase`、`das`、
  `targeted_by`、`badges_all`、`pending_all` 等攻防决策字段。
- **垃圾行**：三态独立进场（灰排队→黄计时→红即将），连击阻挡进场；
  **阻挡规则**（T99 语义）：有暂缓垃圾时攻击全部用于阻挡，队列清空后剩余才发给别人；
  队列上限 12，进场时活动块随堆上推、完全推出场外判负。
- 徽章 = KO 转移（击倒者获被击倒者全部 +1），档位按徽章数 2/6/14/30。
- **节奏相位**：存活比例 50%/30%/10% 切相位，相位内重力缓升、相位间跳变。
- 规则参数见 `ruleset.toml`（随握手下发规则集 ID，两端不一致即拒绝）。

## 确定性

- 内部时间单位 ⅓ns（1 帧 = 5×10⁷ 拍，µs/ms 全整除），位置毫格整数；**内核零浮点**。
- 随机流 per-player splitmix64（seed, pid, 用途派生）；对局 = `(seed, 事件流)` 纯函数。
- `replay` 从 transcript 重放并比对状态 hash，逐位一致才通过（已验证含子进程对局）。
- 锁定时序精度为 1ms 子步（位置由累加器保证精确）——见 ADR-008。

## 已验证

- `cargo test`：workspace 全绿（含 snapshot↔frame roundtrip、内外部程序同局冒烟）
- 校准矩阵（每对 12 局）：null < basic < advanced < expert **严格传递，全胜/全负**
- M/GM 经 5 子代理策略选秀（murder-ball 锦标赛 + 1v1 火力差）重定义（见 `lab/` 与 ADR-026）
- 15 分钟 × 4 人完整对局 headless 耗时 ~7s（release）；99 人局 59 TPS（分帧收集）

## 许可

MIT-0（见 [LICENSE](LICENSE)）。规则取舍与待定项见 `docs/rules/adr.md`。

----
README 版本：20261101（中英文以此号同步；内容不一致时以较新版本号者为准）
