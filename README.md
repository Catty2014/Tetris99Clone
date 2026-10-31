# Tetris99Clone — Tetris 99 复刻（Arena Module for AX1）

确定性 Tetris 99 风格对局引擎 + 无头评测主机 + Bevy 人类客户端 + 内置 Bot 梯队。
同一套物理服务**人类游玩端**与**程序游玩端**：人类客户端只是输入事件的另一种来源，与子进程程序、内置 Bot 走完全相同的校验与物理路径。

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

# 4 人 Bot 对局（basic/advanced/expert/advanced），15 分钟上限，7 秒跑完
./target/release/t99-headless run --players 4 --bots basic,advanced,expert,advanced \
    --seed 31415 --transcript /tmp/m.jsonl

# 逐位回放校验（种子+事件流 → 同 hash）
./target/release/t99-headless replay --transcript /tmp/m.jsonl

# 梯队校准矩阵（每对 N 局，验证档位严格传递）
./target/release/t99-headless calibrate --tiers null,basic,advanced,expert --games 12

# 人类客户端（本地 vs N 个 Bot；--controls arrows 可交换主副操作：方向键移动，WASD 选敌）
cargo run --release -p t99-client -- --seed 42 --bots 3 --tier advanced --controls arrows

# 浏览器版（wasm，WebGL2）
./scripts-build-web.sh                 # 构建 + 胶水 + 剥离 → crates/t99-client/web/
cd crates/t99-client/web && python3 -m http.server 8137
# 打开 http://127.0.0.1:8137/ （点击画布获得键盘焦点）

# C/S（含 B/S）：服务器 = 一局对局；客户端本地镜像重演事件流（确定性，hash 校验一致）
# 对局倒计时按人数自动：4p=5min → 99p=15min（--time-limit-secs 覆盖）；
# --stall-accel 1/0：0 杀 60s 后倒计时按 T/60 倍速燃烧（展览机制，正式 Arena 关闭）
# 规则参数随 hello 下发，客户端镜像与权威引擎必然一致
# bot 数可覆盖全部席位（纯 Bot 局，观战端可在开局前 30s 宽限内入场）
./target/release/t99-server --port 8138 --key t99 --players 4 --bots advanced,advanced --seed 77
# 席位语法：tier（内建）或 ext:label=path（外部程序，与参赛模型提交同通道）。
# 例：M/GM 隐藏档混战（先 cargo build --release -p t99-bot-m）
./target/release/t99-server --port 8138 --key t99 --players 5 --seed 1 \
    --bots "ext:gm=./target/release/t99-bot-gm,ext:m=./target/release/t99-bot-master,expert,advanced,basic"
# 快速分层工作台（引擎直连无节流；输出平均名次/行块/送出/PPS）
cargo run --release -p t99-server --example duel -- --players 5 \
    --bots "ext:gm=./target/release/t99-bot-gm,ext:m=./target/release/t99-bot-master,expert,advanced,basic" \
    --matches 16 --time-limit-secs 120
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id p1 --key t99          # 玩家
cargo run --release -p t99-client -- --server ws://127.0.0.1:8138 --id ob --key t99 --observer --spectate 0  # 观战
cargo run --release -p t99-server --example net_smoke   # 双客户端联机冒烟（镜像一致性断言）
cargo run --release -p t99-server --example analyze -- --players 8 --bots expert,expert,advanced,advanced,basic,basic,null,null --seed 2026 --matches 6
#   ↑ 离线确定性重放：逐帧压力表 + 死亡现场（堆叠/待进垃圾），用于对局诊断
# 对局结束后按 R 请求重开（全员同意或 8s 超时，新种子新局）；无人类席位时结算展示 5s 后自动开下一局
# 注意：release 版客户端从可执行目录解析 assets/（target/release/assets → crates/t99-client/assets）

# 子进程程序参赛（示例：Python 贪心 vs 内置 basic）
./target/release/t99-headless run --players 2 --bots basic \
    --candidate "python3 examples/pybot.py" --seed 99
```

客户端按键：`A/D` 移动（按住连发，引擎侧 DAS/ARR）· `S` 软降 · `空格` 硬降 · `X/J` 顺时针 ·
`Z/K` 逆时针 · `C/Shift` Hold · `Tab` 循环四目标模式 · **方向键导航小窗手动锁敌**。
输入按接收时刻打点（不锁帧渲染循环，粒度≈渲染帧间隔），快照 T99 版 UI：缩放小窗、
危险度框色、死亡灰框冻结、菱形目标指示、落点影、重力外推插值。

## 协议 v3（与草稿 protocol.md 的差异）

- **压放式按键**：`left_down/left_up/...`，DAS/ARR 由引擎统一执行（所有参与方同物理，
  杜绝"程序免费 ARR=1F"的移动特化）；快照含 `das` 字段供程序预测自身移动。
- 事件偏移用整数微秒 `t_us ∈ [0,16666]`；每帧 ≤16 事件、`t` 严格递增、同帧同键（按类别）去重；
  非法 → 该帧空操作。
- **第五目标模式** `target_player`（带载荷 `v:pid`）：手动锁定单个对手。
- 快照含 `phase`（play/clear/are + 剩余毫秒）：消行延迟与 ARE 期间无活动块。
- **节奏相位（ADR-018）**：存活比例 50%/30%/10% 切相位，相位内重力缓升、相位间跳变；
  Clear/Are 延迟相位化取值（ARE 基准 100ms，准则 6F）。
- **垃圾行（T99 忠实实现）**：三态独立进场（灰排队→黄计时→红即将，阶段时长按存活数
  500/1500/2500ms 分档），**连击阻挡**（combo 进行中不进场），消行 1:1 抵消，队列上限 12，
  进场时活动块随堆上推、完全推出场外判负；徽章=KO 转移（击倒者获被击倒者全部+1），
  档位按徽章数 2/6/14/30。快照含 garbage{red,yellow,gray} 与 clearing 行（消除闪烁）。
- 单行 JSON：`hello → ready → (frame → input)* → result`；`ready` 限时 2s，帧响应限时
  16.666ms，连续 600 帧无有效响应判负，进程非零退出判负。

## 确定性

- 内部时间单位 ⅓ns（1 帧 = 5×10⁷ 拍，µs/ms 全整除），位置毫格整数；**内核零浮点**。
- 随机流 per-player splitmix64（seed, pid, 用途派生）；对局 = `(seed, 事件流)` 纯函数。
- `replay` 从 transcript 重放并比对状态 hash，逐位一致才通过（已验证含子进程对局）。
- 锁定时序精度为 1ms 子步（位置由累加器保证精确）——见 ADR-008。

## 已验证

- `cargo test`：25 项单测（SRS 踢墙/T-Spin Double 送出/旋转→硬降成立 T-Spin/垃圾抵消
  落盘/重力整数精确/同种子逐位一致/…）
- 校准矩阵（每对 12 局）：null < basic < advanced < expert **严格传递，全胜/全负**
- M/GM 经 5 子代理策略选秀（murder-ball 锦标赛 + 1v1 火力差）重定义：
  master = B2B Tetris 井型工业（A 场送出 222/局，1v1 火力差对旧档 +117~+123），
  grandmaster = 极限激进档（attack 20/危险阈值 20 行，10 人实杀场送出 1.4-2× 于旧 GM）。
  注意：强手互打常 0 击杀流局，名次指标需在死亡局内解读（见 ADR-026.1 勘误）。
  备选策略档与锦标赛方法在 lab/。
- 跨语言子进程对局 + 逐位回放校验
- 15 分钟 × 4 人完整对局 headless 耗时 ~7s（release）

规则取舍与待定项见 `docs/rules/adr.md`。
