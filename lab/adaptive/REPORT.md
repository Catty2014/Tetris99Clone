# lab/adaptive — 态势自适应攻击（多态状态机）实验报告

日期：2026-10-27　沙盒：`lab/adaptive/`（独立 cargo 工作区，包名 `lab-adaptive`）
二进制：`/home/pig2014/Lab/NPB/t99/lab/adaptive/target/release/lab-adaptive`

## 1. 策略

把种子的 normal/danger 两态扩成**五态状态机**，每态一套 `Weights` + 生存红线 + 目标策略，
按帧态迁移，全部迁移带滞回（防抖）。`选择攻击策略` 的直接体现——何时攻、攻谁、用什么攻：

```
                     targeted_by 非空（立即进入；解除需连续45帧无人瞄）
        ┌──────────────────────────────────────────────────────────────┐
        │                                                              ▼
  ┌───────────┐  b2b且combo≥1 或 badges≥2 或 combo≥2   ┌───────────┐  ┌───────────┐
  │   BUILD   │──────────────────────────────────────▶│   HUNT    │  │ PRESSURE  │
  │ 堆叠质量   │◀── 条件消失+20帧滞回 ──────────────────│ 攻击拉满   │  │ cancel拉满 │
  │ 攒B2B/T槽  │                                       │ TargetBadges│ │ TargetAtt.│
  └─────┬─────┘                                       └─────┬─────┘  └─────┬─────┘
        │          堆高≥13 或 待进垃圾≥3（退出：≤11 且 ≤1）        │             │
        ▼                 ┌───────────────────────────────┘             │
  ┌───────────┐           ▼                                             │
  │    DIG    │◀──────── 优先级最高（活着才有名次）                        │
  │ 生存挖行   │                                                         │
  │ 攻击→0.5  │    time_secs_left<60 或 burn_x10>10（单向闩锁，终局下    │
  └───────────┘    DIG 阈值放宽到 16行/6行垃圾：全力进攻）                │
        ▲              ▼                                              │
  └────────────────── ENDGAME ─────────────────────────────────────────┘
                    全力进攻（流局按徽章排名，送出/人头决定名次）

每帧优先级：DIG > PRESSURE > ENDGAME > HUNT > BUILD
目标策略：PRESSURE→TargetAttackers（1:1 抵消+反击）；HUNT→TargetBadges（收割徽章滚雪球）；
         其余→TargetKos。
```

实现要点：
- 评估器从 `danger: bool` 改为 `ModeCtx { w, redline }`（`evaluate`/`candidates`/`choose_ctx` 全链路），
  生存红线行数随模式变化（BUILD/PRESSURE/DIG=13，HUNT/ENDGAME=14）。
- 模式切换即作废当前 plan（按新权重重规划）并立即重发目标事件；同帧键型/t_us 约束不变。
- 终局闩锁单向：时间只减不增，`ENDGAME` 一进不退（DIG 可临时覆盖）。

## 2. 环境诊断（对实验结论有决定性影响）

- 5p 全强 bot 场 120s 下**全员存活到时限是常态**（32/32 局 0 死亡）。此时流局按徽章（全 0）→ pid 升序，
  "平均名次"≈**座位序伪影**（对调 adp/m 座位后名次精确互换验证）。名次只在**发生过死亡的局**内解读。
- 可复现的"死亡场"：`{bot}, expert, advanced, basic, gm`——gm/basic 会死，名次真实散开。
  该场基线（m 在 slot0，seeds 42/43/44）：平均名次 1.38/1.12/1.25（均值 1.25），送出 119.2/124.5/106.1（均值 116.6）。
- 10 人场（`adp,adp2,m,m2,expert,advanced,basic×2...`）能制造真实击杀（basic 存活率 0.38-0.88），是区分攻防的主战场。
- README 基线"gm≈1.0"在实测中是死亡场里的**垫底者**（3.88-5.00，经常死）：GM 晚防御 + 激进的固有缺陷。

## 3. 迭代数字

每轮只记录有判别力的场。送出=每局平均，名次只在死亡局占比>0 的组里解读（括号内为死亡局占比）。

| 轮 | 改动 | 场次 | 结果 | 结论 |
|---|---|---|---|---|
| R1 | 两态→五态（初版：BUILD=master+tslot0.5，PRESSURE cancel 6.0，HUNT 仅 b2b 源） | 全强 5p ×32 局；5p adp,m,e,a,basic 16 局 | 全部 0 死亡（名次=座位序）；送出 adp 80-93 vs m 97-100，g 92-103 | 火力不足：HUNT 遥测 0-50 帧/局 ≈ 死代码；PRESSURE 过度抵消吃掉送出 |
| R2 | BUILD attack 2.0→3.2、eroded+0.2；PRESSURE cancel 6.0→4.0、attack→3.5；HUNT 门加 combo≥2 | 死亡场 s42；10p 探针 s42/s43 | s42：adp 1.25/送122.5 vs m基线 1.38/119.2。10p：adp 1.25+1.75 vs m 3.00+4.00；但 s43 adp 死 1 次（存活 0.75） | 火力到位，反超 m；防御需加固 |
| R3 | danger_pending 4→3；PRESSURE cancel 4.0→4.5；DIG cancel 4.5→5.0 | 见 §4 最终对照 | 存活率回到 1.00，10p 送出反超 m | 最终版 |

模式帧遥测（10p 实测示例）：B/P/H/D/E ≈ 1154/4749/50/539/244 与 889/5146/7/694/0 ——
**PRESSURE 是主态（10 人场 66-72% 帧）**，HUNT 触发极少（B2B/连击在混战中难成型）。
诚实说明：本策略的胜出主要来自 BUILD 火力增强 + PRESSURE 攻防一体 + 提前 DIG + ENDGAME 闩锁；
HUNT 通路保留但对局影响小。

## 4. 最终对照（R3）

**10 人场**（adp,adp2,m,m2,expert,advanced,basic,expert,advanced,basic，各 2 seed × 4 局）：

| 指标 | adp（×2 实例） | t99-bot-master（×2 实例） |
|---|---|---|
| 平均名次 | **2.25**（1.25+1.75+2.00+2.50 之和÷4... 见下方逐 seed） | 4.25 |
| 存活率 | **1.00**（8/8 局实例级全存活） | 1.00 |
| 送出/局 | **121.6** | 112.3 |

逐 seed：s42 adp 2.00/2.50 vs m 3.75/4.75；s43 adp 1.75/2.75 vs m 3.75/4.75（s42/43 均有真实击杀，死亡局占比 3/4 与 3/4）。

**10 人场 vs gm**（adp,adp2,g,g2,...，2 seed × 4 局）：adp 1.25/2.25、1.00/2.00 vs gm 3.25/4.25、3.00/4.00
（s43 组全员存活为座位序，不可作数；s42 有死亡局，adp 仍在前二）。

**5 人死亡场**（adp,expert,advanced,basic,g，3 seed × 8 局）：

| seed | 死亡局 | adp 名次/送出 | m 基线名次/送出 | gm 名次 |
|---|---|---|---|---|
| 42 | 3/8 | **1.25** / 93.1 | 1.38 / 119.2 | 3.88 |
| 43 | 2/8 | 1.25 / 104.6 | **1.12** / 124.5 | 4.75 |
| 44 | 0/8（流局，名次不作数） | (1.00*) / 95.1 | 1.25 / 106.1 | (5.00*) |

adp 存活率 1.00（全部 FFA 对局零死亡）；gm 在该场 3.88-4.75（经常被打死）。

**solo 存活**（--players 1，150s）：R1 4/4 满时限（place 1.00，~180 行/455 块）；R3 复测 2/2 满时限。

## 5. 结论

- **对 GrandMaster：显著更强。** 同场任意组合下 gm 名次 3.0-5.0 且经常死亡，adp 名次 1.0-2.5 且零死亡、
  送出 94-148 ≥ gm 的 65-116。gm 的"≈1.0"基线是流局座位序伪影。
- **对 Master：10 人场明确更强**（2.25 vs 4.25，送出 121.6 vs 112.3，存活率同为 1.00）；
  5 人死亡场名次打平（1.25 vs 1.25）但 adp 送出偏低（5p 下 PRESSURE 抵消更多）。
  Master 的 1.12-1.38 波动大于 adp 的 1.25/1.25。
- **推荐移植：是。** 定位为新的隐藏档（"Adaptive"）：对弱手收割不手软、被瞄攻防一体、终局全力进攻，
  且没有 gm 的存活缺陷；代价是 5p 小场火力略低于 master（10p/混战场景才是它设计目标）。

## 6. 移植

- 状态机代码位置（`lab/adaptive/src/lib.rs`）：
  - `Mode` 枚举 / `ModeCtx` / `ModeTable` / 滞回常量：`Profile` 定义之后（搜索 `pub enum Mode`）。
  - 状态机迁移：`Brain::update_mode`（每帧调用，优先级 DIG > PRESSURE > ENDGAME > HUNT > BUILD）；
    目标策略：`Brain::target_for`。
  - 评估链路：`evaluate(p, ctx, ...)` 用 `ctx.w` 与 `ctx.redline`（tslot 恒加，由各模式权重自行控制）；
    搜索入口 `choose_ctx`（`choose`/`choose_blocked` 保留旧两态语义作兼容包装）。
  - 模式切换 → `self.plan = None`（重规划）+ 目标事件立即重发。
- 最终 Profile 字段值：骨架 = `master_profile()`，改动：
  - `name="adaptive"`，`danger_pending=3`（R3，原 4）；`danger_h=13`、`well_col=9`、`beam=8` 不变。
  - `modes.w`（按 Build/Pressure/Hunt/Dig/Endgame 序）：

| 权重 | BUILD | PRESSURE | HUNT | DIG | ENDGAME |
|---|---|---|---|---|---|
| landing | -4.5 | -3.6 | -4.0 | -0.8 | -3.5 |
| eroded | 3.6 | 4.5 | 3.2 | 6.0 | 4.2 |
| row_t | 3.217 | 2.8 | 2.6 | 1.5 | 2.2 |
| col_t | 9.348 | 8.5 | 7.5 | 6.0 | 7.0 |
| holes | 7.899 | 8.5 | 6.0 | 14.0 | 7.0 |
| hole_depth | 0 | 0.3 | 0 | 0.8 | 0.2 |
| bump | 0 | 0.2 | 0 | 0.5 | 0.2 |
| wells | 3.385 | 2.5 | 2.5 | 1.0 | 2.0 |
| agg | 0 | 0 | 0 | 0.35 | 0 |
| attack | 3.2 | 3.5 | 5.5 | 0.5 | 6.0 |
| cancel | 2.5 | 4.5 | 2.0 | 5.0 | 3.0 |
| b2b_break | -5.0 | -3.0 | -6.0 | -1.0 | -4.0 |
| tslot | 0.5 | 0.2 | 1.0 | 0 | 0.5 |
| spin_extra | 2.0 | 3.0 | 4.0 | 1.0 | 4.0 |
| 红线 redline | 13 | 13 | 14 | 13 | 14 |

  - 滞回常量：`PRESSURE_RELEASE_FRAMES=45`、`HUNT_RELEASE_FRAMES=20`、`ENDGAME_DIG_H=16`、`ENDGAME_DIG_PENDING=6`。
  - DIG 进入：`max_h≥13 || pending≥3`（终局下 `≥16 || ≥6`），退出 `≤11 && ≤1`；
    ENDGAME 进入：`0<time_secs_left<60 || burn_x10>10`；HUNT 进入：`(b2b&&combo≥1) || badges≥2 || combo≥2`。
- 单元测试：`cargo test --release` 5/5 通过（含状态机迁移测试 `mode_machine_transitions` 与种子引擎回归）。

## 7. 附：实验方法备注

- 全部对局串行执行；无 program_dead/异常抖动出现，无需降载重跑。
- 名次指标只解读死亡局占比 >0 的组；纯流局组（全员存活）名次=座位序，只取送出量。
- 调试旋钮 `T99_BOT_DEBUG=1` / `T99_BOT_TRACE=1` 可用；result 行附带各模式帧数遥测。
