# lab/combo —— 连击链 + 全清猎手 实验报告

日期：2026-10-27 ｜ 沙盒：`lab/combo/`（自 `lab/_template/` 复制，包名 `lab-combo`）
二进制：`/home/pig2014/Lab/NPB/t99/lab/combo/target/release/lab-combo`

## 1. 策略实现

在种子大脑上新增 4 个评分项 + 1 个引擎感知修正（种子只有 `combo_next` 增量与 b2b 项，无「断链罚」概念）：

1. **断链罚（combo_break）**：`combo≥1` 且候选不消行 → 罚 `combo_break × (1 + min(combo,6))`，链越深罚越重（丢 combo 红利 + 垃圾豁免双重损失）；危险档降为弱罚 -2（保命安全网：无路可续时回退正常堆放评估）。
2. **续链奖（combo_keep）**：`combo≥1` 且消行 → 小额正分（主收益已含于 sent 的 combo_next）。
3. **全清奖（allclear）**：AC 候选额外 +20（攻击表 +4 已含于 sent）。
4. **低盘面奖（sparse）**：结果盘面每空一行 +w.sparse（易消结构、逼近全清）；danger 时关闭。
5. **链感知危险（is_danger）**：combo 进行中垃圾被阻挡进场 → pending 不触发危险权重（原版会被 pending 惊扰切求生权重、自断链）；高度红线照常生效。

目标策略按规格：combo 期 TargetKos；被瞄 TargetAttackers；`badges≥2 且链在` → TargetBadges（`harvest_badges=2`）。

## 2. 实验数字

### 2.1 solo 存活（--players 1，150s，4 局，seed 42）

| 版本 | 结果 |
|---|---|
| v1（combo 专用权重） | 4/4 满时长存活（f=7200 time_limit），PPS 3.76 |
| v3（最终） | 4/4 满时长存活，PPS 3.79 |

退化安全网有效：无链/危险时回退堆放评估，全程无叠死。

### 2.2 FFA 5p 对照（120s，--matches 8，combo 固定 seat 0）

| 对照 | seed 段 | 击杀终局/时限流局 | combo 平均名次* | combo 送出 | 对手平均名次* | 对手送出 |
|---|---|---|---|---|---|---|
| vs master | 42 | **0 / 8** | 1.00 | 84.6 | 2.00 | 96.8 |
| vs master | 900 | **0 / 8** | 1.12 | **104.6** | 1.88 | 79.0 |
| vs gm | 42 | **0 / 8** | 1.00 | 88.4 | 2.00 | 100.4 |
| vs gm | 900 | **0 / 8** | 1.00 | 99.4 | 2.00 | 94.5 |

\* 四组全部 32 局均为时限流局（reason=time_limit，逐局 reason 计数核实），无人死亡、徽章全 0 → 名次=pid 升序 tie-break，**名次列是座位序伪影，不作数**。结论只基于三个可靠指标：①同场对照送出量 ②solo 存活 ③连击深度分布。

四段合计送出均值：combo **94.3/局**，master 87.9，gm 97.5。
（另做过 300s 加压实验 6 局：同样 6/6 时限流局、0 击杀；连同诊断轮各非 quiet 运行，直接核验过 reason 的对局 50+ 局全部 time_limit、0 击杀。）

### 2.3 关键诊断（改变了结论解读）

- **所有对局 0 击杀、名次=座位序伪影**：最终表 4 组共 32 局逐局核验 reason，全部 time_limit（300s 加压 6 局同样全员存活）。5 个强 bot 的消行 1:1 抵消吸干了全部攻击。对照实验（无 combo 的 gm/master 同场）得 gm@seat0=1.00、master@seat1=2.00，与任务书基线数字完全一致；把 combo 换到 seat 4 后立刻变为 4.75 —— 证实名次随座位而非强度变化。**故平均名次不作为强度证据，结论只基于：①送出量（同场对照）②solo 存活 ③连击深度分布。**
- **真指标 = 送出量**（唯一与座位无关的进攻指标）：combo 与 master 打平（±噪声），稳定低于 gm 约 3%。
- **连击链深度上不去（瓶颈）**：内置诊断 `maxcombo` 全程只有 **2-3**（389 块中消行锁定约 145、无消行约 244）。加成表要 combo≥9 才到 +5/消，深度 2-3 时链收益≈0-2/消。通用的"消行+低盘面"权重造不出可持续链所需的阶梯/易消结构（hole 罚与平坦性权重天然排斥它），且 cancel 会吃掉消行流使被瞄时送出骤降（seat4 实测 61-63 vs gm 同场 126-145）。
- 调参轨迹：v1（sparse0.8/attack2.2）sent 80.6-89.2 → v2（attack4.6/sparse0.2/beam12）85.2 → v3（attack6.0/well_keep0.6/AC20/sparse0）87.3-104.6 → v4（cancel0.8）seat4 无改善（63.0 vs 61.0），弃用。

## 3. 最终对照结论（仅基于可靠指标）

| 可靠指标 | combo（本策略） | master | gm |
|---|---|---|---|
| ① 送出/局（4 段均值） | 94.3 | 87.9 | **97.5** |
| ① 送出稳定性 | 84.6-104.6 | 79.0-96.8 | 94.5-100.4 |
| ② solo 150s 存活 | 4/4 满时长 | 未测（种子档） | 未测（种子档） |
| ③ 连击深度 maxcombo | **2-3（全程）** | — | — |
| 流局占比（120s×8×4 组） | 32/32 time_limit | 0 击杀 | 0 击杀 |

（名次指标因全员存活不可作数，见 2.3。）

**是否推荐移植：不推荐。** 理由：(1) 在该引擎的强 bot 同场环境里从未产生击杀，名次指标退化为座位序，combo 的两大设计红利（链期免伤、深链滚雪球）都无从兑现；(2) 唯一真指标送出量上 combo ≈ master、略逊 gm（-3%），未证明更强；(3) 结构性瓶颈明确：通用评估权重造不出能撑起 combo 加成表（深度 9+ 才到 +5）的阶梯结构，实测链深封顶 2-3。

若要真正做强连击族，需要的不是调权重，而是**专用结构构建器**：显式识别/搭建阶梯易消结构（目标 pattern 相似度评分，而非通用 hole/bump 罚），起链后用 pattern 匹配续链；工作量超出本轮范围，建议单独立项。

## 4. 移植附录（若未来采用）

全部改动位于 `lab/combo/src/lib.rs`（自包含，未动仓库其余路径）：

- `Weights` 新增 4 字段（L511-517）：`combo_break: f64`、`combo_keep: f64`、`allclear: f64`、`sparse: f64`（master/gm 档全部填 0.0，行为不变）。
- `evaluate()` 新增评分块（L781-797，位于 tslot 项之后、生存红线之前）：
  - `if fr.combo >= 1 { cleared==0 → sc += w.combo_break*(1.0+min(combo,6)); else sc += w.combo_keep }`
  - `if allclear { sc += w.allclear }`
  - `if !danger { sc += w.sparse * (ROWS - filled_rows) }`
- `is_danger()`（L831）：`max_h ≥ danger_h || (combo < 1 && pending ≥ danger_pending)` —— 链进行中 pending 不触发危险。`choose_blocked` 与 `Brain::decide` 调试处均改用此函数。
- 最终 Profile（`profile()`，L1443）：`name:"combo", beam:12, danger_h:13, danger_pending:5, harvest_badges:2, well_col:9`；
  `w`: landing -4.0, eroded 3.0, row_t 2.6, col_t 7.5, holes 6.5, hole_depth 0.1, bump 0.3, wells 1.2, agg 0.08, attack 6.0, cancel 2.0, b2b_break -6.0, well_keep 0.6, tslot 0.8, spin_extra 3.5, **combo_break -9.0, combo_keep 1.0, allclear 20.0, sparse 0.0**；
  `w_danger`: landing -0.8, eroded 6.0, row_t 1.5, col_t 6.0, holes 14.0, hole_depth 0.8, bump 0.5, wells 1.0, agg 0.35, attack 0.8, cancel 4.0, b2b_break -1.0, well_keep 0.0, tslot 0.0, spin_extra 1.0, **combo_break -2.0, combo_keep 0.0, allclear 0.0, sparse 0.0**。

工程合规：串行跑测、全程无 program_dead/异常；单测 4/4 通过（TSD 候选识别与偏好不回归）；decide() 常规帧远低于 3ms 预算。
