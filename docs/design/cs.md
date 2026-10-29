# t99 Server / Client / HeadlessClient 通信架构

状态：草案（目标架构，非现状描述）
日期：2026-10-29
范围：`t99-server`、`t99-client`、`t99-hc`（HeadlessClient）、候选程序契约

## 0. 目标

1. **一条信道，三种订阅。** Client 与 HeadlessClient 使用同一个 WebSocket 端点、同一套握手和消息格式，`profile` 决定服务器发什么。
2. **权威与表现解耦。** 服务器逻辑固定 60Hz；输入采样随显示帧率、显示、对手视图各自独立于逻辑，不互相推进。
3. **程序席位不承担对手计算。** HeadlessClient 只拿自己的 `state` 和 5Hz 的 `summary`，不重演其他 98 席。
4. **Client 承担完整观战。** 本地重演 99 席，对手显示默认 30Hz、可调 5–60Hz。
5. **去掉逐席串行等待。** 服务器按帧收集所有席位的输入，最坏等待为一个共享 deadline，而不是 `sum(RTT)`。
6. **99 个外部程序席位可跑到 60 TPS。** 当前 `5229c86` 的基线是全网络 99p 34 TPS、CPU 27%。

## 1. 现状与问题

当前实现（`5229c86`）：

- 服务器每连接一个阻塞 reader + 一个 mpsc writer，已去掉 2ms 读轮询；空闲 99 连接 CPU 从 91.2% 降到 0%。
- 对局循环对每个网络席位调用 `NetPlayerAdapter::act`：发私有 `frame`，阻塞等 `input n`，单席 deadline 16ms。99 席串行，约 29ms/帧，34 TPS。
- `FrameMsg::from_snapshot(snap, 0)` 把 `player_id` 写成 0。
- 客户端 `t99-client` 在 Bevy `FixedUpdate` 里同时做输入 drain/send 和 mirror 推进；输入采样与渲染、逻辑耦合。
- `t99-bot-m run_ws`、`t99-ws-bot`、`t99-client` 各自复制协议与镜像逻辑。
- 服务器 `history: Vec<String>` 保留全部 tick 供 observer 补史；15 分钟局是持续增长的内存与重放开销。
- `FRAME_MS=16`，引擎实际帧长 16.666ms。

这些问题的共同点是：**把“服务器要什么”和“客户端怎么显示”混在同一个帧往返里。**

## 2. 角色与 crate

| 角色 | 计算范围 | 订阅 |
|---|---|---|
| Server | 99 席权威 | — |
| Client native | 本地重演 99 席 + 渲染 | `tick` 60Hz |
| Client wasm | 重演或 digest | `tick` 60Hz / `digest` 30Hz |
| HeadlessClient | 自己 + 全局摘要 | `state` 60Hz + `summary` 5Hz |
| Observer | 同 Client，无输入 | `keyframe` + `tick` |
| Candidate | HeadlessClient + Policy，或自实现 wire | 同 HeadlessClient |

目标 crate 切分：

```text
t99-core            确定性 Match / Snapshot / Ruleset
t99-proto           wire DTO，移除 adapter
t99-session         Transport trait、握手、时钟同步、InputQueue、stats、resync
t99-session::mirror 可选 feature：按 tick 重演整局
t99-client-native   session + mirror + Bevy UI
t99-client-wasm     session + mirror/digest + web UI
t99-hc              session + Policy，不编译 mirror
```

- `Client = SessionCore + Mirror + UI`
- `HeadlessClient = SessionCore + Policy`
- `Observer = Client` 的无输入模式
- `SubprocessAdapter` 改为 `StdioTransport`，与 WS 走同一个 session 语义

## 3. 时钟域

| 域 | 值 | 驱动 | 约束 |
|---|---:|---|---|
| logic | 60Hz，16667µs | Server | 唯一权威；`frame_n` 是全局时钟 |
| input sampling | = 显示帧率 | Client | 随 vsync / framerate；采样时打 `t_us`，不按逻辑帧打点 |
| render | vsync / 不限帧 | Client | 只读逻辑状态并插值，不推进 `Match` |
| opponent view | 30Hz 默认，5–60 可调 | Client | 从 mirror 采样；digest 模式下为订阅参数 |
| summary | 5Hz | Server → HC | 堆叠、KO、徽章、目标信息 |
| own state | 60Hz | Server → HC | 自己盘面的权威快照 |
| tick | 60Hz | Server → Client | mirror 重演的唯一输入 |

### 3.1 逻辑

- 固定 60Hz，用 `Instant` 累加 `16_667µs`，不再 `sleep(16ms)`。
- `FRAME_US=16667`，`FRAME_DEADLINE` 约 15ms。
- 掉帧补算但不跳帧；`frame_n` 单调递增。

### 3.2 输入

- 输入采样率等于显示帧率（`vsync` 或 `framerate` 决定），不单独配置 `sample_rate`；显示高于 60 时可以高于逻辑帧率，但不产生更多消息。
- 一个逻辑帧窗口内的多次采样编码成多个 `t_us` 事件；每逻辑帧最多一个 `input` 批次。
- 例：采样 240Hz、逻辑 60Hz，一帧 4 个事件，`t_us` 分布在 0–16666。
- 采样写入带时间戳的 ring；逻辑帧边界只做“取走并封装”，不阻塞采样。
- Client 的本地回显只能是 UI 预测，不能回灌 `Match`。

### 3.3 显示

- `vsync = true` 时以显示器刷新率为准，`framerate` 不生效；`vsync = false` 时 `framerate` 是上限。
- 渲染不调用 `engine.frame`，只读最近两个逻辑状态做插值。
- 自己的盘面 60Hz；对手缩略图默认 30Hz，featured 视图可 60Hz。
- 输入采样随显示帧率，但不推进逻辑；渲染卡顿只会降低采样密度，不改变已采样事件的 `t_us` 时序。

### 3.4 HeadlessClient

- 由 `state` 到达驱动，60Hz。
- 内部可以更高频规划，但提交仍按 `n_intent` 批量。
- 不跑渲染循环，不编译 mirror。

## 4. 对手速率

### 4.1 结论

- **对手显示默认 30Hz，范围 5–60Hz，玩家可调。**
- **summary 固定 5Hz。**
- **权威逻辑始终 60Hz。**

### 4.2 依据

当前 ruleset 的阶段参数：

| 阶段 | clear_delay | ARE | 消行后锁定间隔 | 无消行锁定间隔 | 垃圾进场 |
|---|---:|---:|---:|---:|---:|
| P1 | 300ms | 100ms | 400ms / 2.5Hz | 100ms / 10Hz | 1200ms |
| P2 | 250ms | 100ms | 350ms / 2.9Hz | 100ms / 10Hz | 900ms |
| P3 | 200ms | 80ms | 280ms / 3.6Hz | 80ms / 12.5Hz | 650ms |
| P4 | 150ms | 60ms | 210ms / 4.8Hz | 60ms / 16.7Hz | 450ms |

- 堆叠、KO、徽章、垃圾计数的变化受 clear_delay、ARE、garbage_stage 限制，5Hz 足够，约 200ms 的陈旧度小于一次消行动画。
- 但盘面不止静态堆叠：客户端能看到下落块。活动块位置每帧变化，5Hz 会明显跳格；30Hz 足够连续，60Hz 为精确值。
- 因此 5Hz 只用于 **summary**，不用于对手盘面显示。

### 4.3 两种客户端模式

1. **Mirror 模式（native 默认）**：`tick` 60Hz 在本地重演整局，渲染时按 `opp_view_hz` 采样。带宽不增加；客户端 CPU 是整局模拟。observer 99 席实测 812 FPS，说明模拟不是瓶颈。
2. **Digest 模式（wasm / 弱设备 / 纯观战）**：服务器发自己的 `state` 60Hz + 对手 `digest` 30Hz，客户端不模拟对手。
   - `digest` 若包含活动块，必须 30Hz。
   - 5Hz 的 digest 只能作为静态堆叠缩略图，不能声称支持下落动画。
   - 99 席 × 约 250B × 30Hz ≈ 700KB/s/observer，只适合少量观战端，不适合 99 个参赛席位。

## 5. 协议

### 5.1 握手

服务器在鉴权、席位分配完成后先发 `hello`：

```json
{"type":"hello","protocol":7,"ruleset":"9f3c1a7e","match_id":123,"seed":20261029,
 "players":99,"seat":7,"role":"player","profile":"program",
 "logic_hz":60,"lead_frames":2,"tick_hz":60,
 "summary_hz":5,"opp_view_hz":30,
 "time_limit_secs":300,"stall_accel":false}
```

- `protocol` 是整数，表示 wire 语义；`ruleset` 是短 UUID 字符串，只表示规则集身份，不展开规则内容。
- 官方实现只支持一种 `protocol + ruleset` 组合；开发构建可以维护一个 allowlist，但正式构建不协商、不降级、不猜测。
- 客户端必须精确匹配。不认识或不接受时，立即回 `error {code:"unsupported_version"}` 并关闭连接，**不能发 `ready`，不能进入对局**。
- 接受时回：

```json
{"type":"ready","protocol":7,"ruleset":"9f3c1a7e"}
```

- 服务器再次校验；不匹配同样关闭连接。`ready` 超时 2s 视为拒绝。
- 在 `ready` 之前服务器不发 `tick`、`state`、`summary` 或 `frame`。
- `ruleset` 应对规则集内容做确定性派生（例如 canonical ruleset 的 UUIDv5 短串），同一规则集在整届比赛中保持稳定；不要每次启动随机生成。
- 参赛者在整个参赛周期内只会看到一种组合；`ruleset` 变化必须换版本并重建客户端镜像。

`profile` 取值：`program`、`human`、`observer`。`lead_frames` 写进 ruleset，所有程序席位一致。

### 5.2 HeadlessClient 订阅

```json
{"type":"state","n":1234,"player_id":7, "...": "Snapshot 字段"}
```

`player_id` 必须是真实席位，不能硬编码 0。

```json
{"type":"summary","n":1234,
 "alive":[0,1,1,"..."],"ko":["..."],"badges":["..."],
 "height":["..."],"holes":["..."],
 "targeting":[7],"target":42,"target_mode":"kos","time_secs_left":120}
```

`summary` 不包含对手棋盘，只给目标选择所需的压缩信息。`height`/`holes` 足以支撑 kos、badges、attackers 的目标评估。

### 5.3 Client 订阅

```json
{"type":"tick","n":1234,"in":[[7,[[0,"left_down"],[4000,"hard"]]]]}
```

保持当前格式。中途加入先发 `keyframe`，再发后续 tick，不再重放全部历史。

### 5.4 上行

```json
{"type":"input","n":1236,"events":[{"t_us":0,"k":"left_down"}]}
```

- `n` 是意图帧，必须满足 `frame_n < n <= frame_n + lead_frames`。
- 过期或越界整批丢弃并计 miss。
- `profile=program` 每帧必须发，允许空 `events`；`profile=human` 可以不发，视为空输入。
- `target_player` 额外带 `v`。

### 5.5 其他消息

| 消息 | 方向 | 用途 |
|---|---|---|
| `keyframe` | S→C | 每 5s 或按需；完整对局状态 |
| `digest` | S→C | 5/30Hz 对手盘面，仅 digest 模式 |
| `resync` | C→S | 请求 keyframe + 后续 tick |
| `result` | S→C | ranking：player_id/seat/rank/badges；逐席 ko/lines/sent/pieces |
| `error` | S→C | auth / id_invalid / match_running / no_seat |
| `ping/pong` | C↔S | JSON `t`；RTT/丢包/HUD |

WS 控制帧 `Ping` 当前不再自动回 pong；要么由写线程统一处理，要么在协议层禁用并只保留 JSON ping。

### 5.6 身份、结算与排名

- `seat` 是 0..N-1 的整数席位；`player_id` 是字符串身份，两者分开。
- PlayerID 在连接时查重，重复直接拒绝；席位从空闲席位随机分配，每局重新洗牌，不设 roster 顺序。
- 结算向 stdout 输出一行 JSON，日志继续走 stderr：

```json
{"type":"match_result","match_id":123,"seed":123456,"reason":"time_limit",
 "ranking":[
  {"player_id":"A","seat":7,"rank":1,"badges":12,"alive":true},
  {"player_id":"B","seat":31,"rank":1,"badges":12,"alive":true},
  {"player_id":"C","seat":2,"rank":3,"badges":9,"alive":true},
  {"player_id":"F","seat":88,"rank":4,"badges":4,"alive":false}
 ]}
```

排名规则：

1. `last_alive`：胜者 `rank = 1`；其余按淘汰序从后往前排，最后被淘汰者 `rank = 2`。
2. 流局/时间到：幸存者按徽章降序；同徽章同名次；采用 competition 排名，即 `1,1,3,3,5`。
3. 流局中的已淘汰者排在所有幸存者之后，按淘汰序从后往前排；起始名次为 `幸存者人数 + 1`。
4. `badges` 对已淘汰者取被击倒时的值，对幸存者取终局值。
5. 同一帧多人淘汰同名次，并让 competition 的下一位跳过该组。例如 48、47 名在同一 tick 死亡，则两人都是 `#47`，下一组从 `#49` 开始。
6. 实现上按 `death_frame` 分组，从最后一组向前处理：组内同名次，下一组 `rank += 组人数`。正常结束从 `#2` 开始；流局从 `幸存者人数 + 1` 开始。

## 6. Server 帧循环

```text
loop:
  t0 = Instant::now()

  drain reader_rx -> pending[n_intent]
  inputs = pending.remove(frame_n + 1)

  engine.frame(&inputs)

  tick: Arc<str> = serialize once
  for client in mirror_subscribers: enqueue_shared(tick)

  for hc in program_seats: enqueue(state(pid))

  if frame_n % 12 == 0:
      for hc: enqueue(summary)

  if frame_n % 300 == 0:
      keyframe()

  sleep_until(t0 + 16_667us)
```

要点：

- 没有 `adapter.act`；收集是 O(连接数) 的 drain，最坏等待一个共享 deadline。
- `tick` 每帧只序列化一次，用 `Arc<str>` 或共享 `Bytes` 扇出。
- `state` 仍逐席序列化；99 个程序席位下这是主要剩余 CPU，后续可用二进制或增量压缩。
- 慢客户端：队列超限就断开，重连走 `keyframe` + 后续 tick；tick 不能静默丢弃。
- 程序席位：连续缺 `input` 超过 grace 计 miss，超过 600 帧判 `program_dead`；人类不惩罚。

## 7. Client / HeadlessClient 内部

### 7.1 SessionCore

- `Transport` trait：native WS、wasm WS、stdio。
- 握手、`protocol` / `ruleset` 精确校验、`frame_n` 估计、RTT/丢包统计。
- `InputQueue`：带时间戳的 ring；按逻辑帧边界封装 `input`。
- `resync`：检测 tick 不连续或 hash 不匹配时请求 keyframe。

### 7.2 Client

- `Mirror` 线程按 tick 顺序推进 `Match`；不连续即 `resync`。
- 渲染读取最近两个状态插值；对手视图按 `opp_view_hz` 采样。
- 输入采样随显示帧率（无独立 `sample_rate`），与逻辑 `FixedUpdate` 解耦。
- HUD 读 session stats，不反推协议状态。

### 7.3 HeadlessClient

- `state` → `Policy::act(&ProgramView)` → 事件写入 `InputQueue`，目标帧 `n + lead_frames`。
- `summary` → 更新目标选择、威胁评估。
- 不订阅 `tick`，不编译 `Mirror`。
- 候选程序只需实现 `Policy`，或自行实现 wire protocol。

## 8. 迁移顺序

### P0 正确性

- `FrameMsg::from_snapshot(snap, snap.pid)`。
- `FRAME_US=16667` + 累加器，替换 `sleep(16ms)`。
- WS 控制帧 ping 策略定案。
- 人类 lobby 输入接 `state` 和 `n_intent`。

### P1 输入与状态解耦

- 引入 `ClientSession`，上行 `input(n_intent)`，服务器按帧 drain。
- 删除 `NetPlayerAdapter::act` 的逐席等待；`SubprocessAdapter` 映射到同一 transport 语义。
- `tick` 共享序列化 + 有界队列。
- 验收：99p 全网络 ≥55 TPS，CPU 不高于当前 27% 的 1.5 倍，无 setup 失败。

### P2 抽 core 与 HC

- `t99-session` + `Transport`。
- `t99-hc` 二进制 + `Policy` trait。
- 迁移 `t99-bot-m run_ws`、`t99-ws-bot`，删除重复协议/镜像。
- `Match` keyframe 编解码。

### P3 Client

- native client 改为 session + mirror + UI。
- 输入采样随显示帧率、与逻辑解耦；渲染插值；对手视图 30/60 可调。
- observer 走 keyframe + tick。

### P4 wasm 与候选契约

- wasm WebSocket transport。
- digest 模式 30Hz（含活动块）/ 5Hz（纯堆叠）。
- 候选 SDK + Grader 拉起进程、提供 URL/seat/key；stdio 只是 transport 选项。

### P5 性能与健壮性

- `state` 二进制/增量编码。
- 每客户端压缩策略。
- resync、慢客户端、指标。

## 9. 风险与未决

1. **keyframe 完整性。** 现有 `Snapshot` 未必覆盖 `Match` 的全部内部状态，尤其是 pending garbage 顺序。需要扩展 `Snapshot` 或实现 `Match::encode_state`；否则 keyframe 只能恢复画面，不能作为精确重演起点。
2. **确定性。** 引擎只在 ruleset 推导处使用 `f64`（`match_ticks_for`），逻辑本身是整数 tick；`protocol` 与 `ruleset` 都必须严格匹配。wasm 与 native 的 f64 行为一致，但版本漂移仍要拒绝镜像。
3. **`lead_frames` 公平性。** 正式赛固定为 2 帧，写进 ruleset；所有程序席位一致，不按 RTT 单独放宽。
4. **`state` 序列化成本。** 99 个程序席位每帧 99 份 JSON 是 P1 之后的主要 CPU；先用基准测量，再决定二进制或增量。
5. **tick 连续性。** mirror 不能丢 tick；慢客户端只能断开并 resync，不能静默降采样。
6. **对手 digest 带宽。** 30Hz 含活动块的 digest 约 700KB/s/observer，不适合大规模人类客户端；native 默认 mirror 更省带宽。
7. **WS 控制帧。** reader/writer 分线程后，tungstenite 自动 pong 写入 reader 缓冲，不会被 writer flush；当前协议用 JSON ping，需在文档和客户端中明确。

## 10. 与现有代码的映射

| 现状 | 目标 |
|---|---|
| `t99-proto::NetPlayerAdapter` | `t99-session` 的 WS transport + server `ClientSession` |
| `t99-proto::SubprocessAdapter` | `t99-session` 的 stdio transport |
| `NetPlayerAdapter::act` 串行 | 帧循环 drain `input(n_intent)` |
| `FrameMsg::from_snapshot(snap, 0)` | `state`，`player_id = pid` |
| `t99-client` FixedUpdate drain+send | SessionCore + 独立 InputQueue + 渲染插值 |
| `t99-bot-m run_ws` / `t99-ws-bot` | `t99-hc` + `Policy` |
| `history: Vec<String>` 全量 | `keyframe` 每 5s + 最近 tick |
| `FRAME_MS=16` | `FRAME_US=16667` 累加器 |

## 11. 实现进度

截至 2026-10-29，`cs-arch` 分支：

- **P0 已完成**（`f946c34`）：服务器墙钟改为绝对 deadline，300 帧实测 4.9835s、平均 16.6672ms；`FrameMsg` 使用真实 `snap.pid`；人类 lobby 客户端回 `ready` 并按私有 `frame.n` 回 `input`。
- **P1a 已完成**（`5fdf3e9`）：`PlayerAdapter` 增加 `begin_frame`/`try_input`/`end_frame`，服务器先全量发帧，再用共享 deadline 收集；99 席全网络 300 帧实测 59.07 TPS、65.8% 单核、0 setup 失败。
- **P1b 待做**：`input(n_intent)` 事件流与 `lead_frames`，去掉私有 `frame` 请求/应答；WAN 下当前 16ms deadline 仍会丢帧。
- **P2 待做**：`t99-session` 抽取、`t99-headless connect`、`Policy`。
- **P3/P4 待做**：Client mirror/输入采样解耦、wasm/digest、候选 SDK。

性能对照（同一 99p harness，5s 局）：

| 版本 | TPS | 单核 CPU | 说明 |
|---|---:|---:|---|
| 旧 2ms 轮询，串行 act | 4.03 | 54.1% | 74.47s |
| `5229c86` reader/writer | 34.17 | 27.0% | 8.78s |
| P1a phase-split | 59.07 | 65.8% | 5.08s，接近 60 |

P1a 的 CPU 上升是因为真正跑满了 60 TPS，且每帧多了一次 99 席轮询；CPU/帧约 11ms，仍低于 16.7ms 预算。

## 12. 配置分层

配置格式固定为 TOML。官方 Rust 程序/库按以下顺序取值：

```text
CLI > 环境变量 > 配置文件 > 内建默认值
```

1. 命令行：`--server-url`、`--key`、`--player-id`、`--config` 等。
2. 环境变量：`T99_SERVER_URL`、`T99_SERVER_KEY`、`T99_PLAYER_ID`；嵌套键用双下划线，例如 `T99_DISPLAY__VSYNC`。
3. 配置文件：server 缺省 `./server.toml`，client 缺省 `./client.toml`；`--config` / `T99_CONFIG` 覆盖。规则集单独用 `--ruleset` / `T99_RULESET`，缺省 `./ruleset.toml`。

合并按字段进行，不整表替换；数组整体替换，不追加。未知字段在配置文件中告警，在 CLI 中视为错误。`--config` 本身只能来自 CLI 或环境变量，不能由配置文件指定。

静态配置与运行时清单分开：

- `server.toml`：`[server]`、`[auth]`、`[match]`，只由服务器读取。
- `client.toml`：`[client]`、`[control]`、`[display]`，只由官方 Client 读取。
- `ruleset.toml`：只读规则集与阶段表，server/client/HeadlessClient 共用；不能由运行时配置覆盖。
- 运行时清单：由 Grader 为每个参赛进程生成，包含 `server.url`、`key`、`player_id`、期望的 `protocol`/`ruleset` 等；写入临时文件，并同时以 CLI 和环境变量传入，三份内容一致。
- `key` 是所有参赛者共享的房间口令，不是秘密，也不是身份凭证；文件权限、argv 和日志暴露不作为安全边界。身份由 `player_id` 和 allowlist 负责；若需要防止冒用，另加签名或 per-player token。

参赛程序（含 bot）在运行时总会同时收到三种来源，内容一致，可以任选其一读取。自己实现 wire protocol 的程序只需要 `server.url`、`key`、`player_id` 这一小组；`[control]`、`[display]` 主要服务官方 Client。协议与规则集的最终权威仍是服务器 `hello`，运行时清单里的 `protocol`/`ruleset` 只用于提前自检。

官方库建议提供：

```rust
let cfg = t99_config::Config::load()?;      // CLI > env > file > default
let rt = cfg.runtime();                     // url / key / player_id / expected versions
```

## 13. 非目标

- 不向 99 个玩家广播对手棋盘快照。
- 不把服务器权威模拟降到 30/5Hz。
- 不要求候选程序实现渲染或 mirror。
- 不把 stdio 作为与 WS 不同的语义路径，只是 transport 不同。
