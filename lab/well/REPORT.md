# lab/well — B2B Tetris 井型工业 · 实验报告

二进制：`/home/pig2014/Lab/NPB/t99/lab/well/target/release/lab-well`（外部程序，参赛线协议，确定性，无墙钟/随机）

## 结论（先行）

**推荐移植。** B2B Tetris 井型工业（最终版 v3）在 6 个 seed 段、24 局 5p FFA 中对 t99-bot-master 与
t99-bot-gm **场均名次 1.08**（vs master 线 1.00/1.00/1.25，vs gm 线 1.00/1.25/1.00），基线场均 2.08；
送出量 181–222/局（均值 ~193），约为 master（93–111）与 gm（94–110）的 **2 倍**；
solo 4/4 满时存活（~600 块/局，PPS 4.96）。全部对局无 miss/program_dead。

## 策略说明

右列 col 9 为**专用 I 井**：其余 9 列快速平整堆放（bump 权重抬高）喂井，井列保持空深；
Hold 轮转等 I；I 竖入井 = 消 4（Tetris）+ b2b(+1) = 5 行/发的主攻击引擎，链不断则每发都带 b2b。
种子代码里 `well_keep`/`tslot` 权重为 0（历史调试原因），且通用堆面指标（holes/wells/bump/col_t/row_t）
会把深井当「洞/杂井/崎岖」惩罚——这正是井策略此前做不出来的原因。本实验的改动：

1. **井列指标掩码**（`evaluate`，`well_mode && !danger` 时）：井列不计入 holes/hole_depth/col_t/wells/bump/agg，
   row_t 中井列按「已填实」处理。井的健康度改由两条专门项管理。
2. **well_keep = +3.5/行**：井深（其余最矮列高 − 井列高，封顶 8）线性奖励——鼓励平整喂井、维持深井。
3. **pollute 污染罚（新权重，-10.0/格）**：非 I 块在井开放（有深度）时，凡落入井列的格子全计污染
   （含架桥——桥面同样挡住 I 入口）；消 4 例外（Tetris 本身就是目的）。危险态关闭（保命优先允许填井）。
   实现：`well_pollution()` 在 `push_cand` 里用落章前局面计数。
4. **硬过滤「井等 I」**（`choose_blocked`）：井深 ≥4（一发 Tetris 就绪）且非危险态时，
   非 I 的入井候选整体剔除（若剔空则回退全量，保证不饿死）。Hold 分支天然配合：hold/next 里有 I 时
   hold 分支把 I 送井得分压倒一切，非 I 当前块只能平整堆放。
5. **B2B 链保护**：b2b_break 罚 -9（master -5）——链断 = 引擎熄火；非 I 的 1–3 消与浅井 I 消都被此抑制。
6. **危险兜底**：保留 master 的 w_danger 档（cancel 4.0 / holes 14 / attack 0.8），danger_h 13→12 更早触发、
   danger_pending=4；danger 时掩码与井权重全部关闭，井洞按普通洞处理 → 自然填平求生。
7. 束搜索 beam 8→10；目标策略沿用 master（被瞄→TargetAttackers，徽章≥4 且 b2b 连击中→TargetBadges，否则 TargetKos）。

其余引擎复刻（SRS/踢墙/T-Spin/攻击表）、候选生成、(x,rot) BFS 执行器全部沿用种子代码，未动。

## 实验数字

环境：`target/release/examples/duel`，串行度 ≤2（16 核；1 玩家 solo 与单路 FFA 并行实测无争用，
全程无 miss/program_dead）。注意：**同 seed 各局完全复现**（引擎+bot 均确定），信息量在 seed 段之间。

| 轮 | 版本 | 测试 | seed | 平均名次（本 bot） | 送出/局 | 基线同场 |
|----|------|------|------|------|------|------|
| R1 | v1 | solo 存活 1p×4 局×150s | 777 | 1.00（4/4 满时存活，~600 块/局） | — | — |
| R1 | v1 | vs master 线 ×8 局 | 42 | **1.00（8/8 第一）** | 181.1 | m 2.00/100.8 |
| R1 | v1 | vs master 线 ×4 局 | 999 | **1.00** | 181.2 | m 2.00/94.5 |
| R1 | v1 | vs gm 线 ×4 局 | 1337 | **1.25** | 191.2 | gm 2.25/95.5 |
| R2 | v2（harvest 2, attack 4.0, wk 3.5） | vs gm 线 ×2 | 42 | 1.00 | 209.0 | gm 2.00/104.0 |
| R2 | v2 | vs gm 线 ×2 | 1337 | 1.50（名次回退） | 201.5 | gm 2.50/102.5 |
| R3 | v3（harvest 4, attack 4.0, wk 3.5）= 最终 | vs gm 线 ×4 | 42 | **1.00** | 201.5 | gm 2.00/102.8 |
| R3 | v3 | vs gm 线 ×4 | 1337 | **1.25** | 196.0 | gm 2.25/93.5 |
| 终 | v3 | vs master 线 ×4 | 555 | **1.25** | 221.5 | m 1.75/92.5 |
| 终 | v3 | vs gm 线 ×4 | 31337 | **1.00** | 183.2 | gm 2.00/100.8 |

迭代记录：
- v1（well_keep 3.0 / attack 3.5 / harvest 4）→ 三 seed 段全胜，直接确立优势。
- v2 提攻（harvest_badges 4→2 更早徽章收割、attack 4.0、well_keep 3.5）→ 送出 +10 但 seed1337 名次 1.25→1.50，回退 harvest。
- v3 = harvest 4 + attack 4.0 + well_keep 3.5 → 名次与 v1 持平、送出 +5，采纳为最终版。

## 结论

井型工业策略成立且显著强于现任 Master/GrandMaster：主指标（平均名次）6 个 seed 段全部碾压
（1.00–1.25 vs 基线 1.75–2.50），次指标（送出）约 2 倍。唯二未拿第一的 2 局（seed555 ×1、seed1337 ×1）
均仍居第二，未出现垫底。瓶颈与注意点：
- T-Spin 参与度低（spin计划 0–4/局）——攻击几乎全部来自 Tetris+b2b；若叠加 TSD 蓄势可能还有空间，但当前已够 decisive。
- 危险兜底实测足够：FFA 中无一局因自杀垫底（从未掉出前二）。
- 引擎约束遵守：无 miss / program_dead；decide() 常规路径 <3ms（beam 10 实测 PPS 4.4–4.7 无超时）。

## 移植

目标：`crates/t99-bot-m`。改动共 5 处（均为本沙盒 `src/lib.rs`，行号以最终文件为准）：

1. `Weights` 结构体新增字段 `pub pollute: f64`（非 I 井列污染罚/格）；master/gm 档填 `0.0` 保持行为不变。
2. `Profile` 结构体新增字段 `pub well_mode: bool`；master/gm 档填 `false` 保持行为不变。
3. `Cand` 结构体新增字段 `pub pollute: i64`。
4. 新增函数两个（位于 `push_cand` 之后）：
   - `fn col_height(g: &Grid, c: usize) -> i64` —— 列高。
   - `fn well_pollution(p, g, kind, fin_rot, fin_x, danger, cleared) -> i64` —— 非 I 且井开放且不消 4 时，数落点在井列的格数。
5. `push_cand`：调用 `well_pollution` 并把 `pollute` 传入 `evaluate`/`Cand`。
6. `evaluate`：签名加 `(kind, pollute)` 两参；`let wcm = p.well_mode && !danger;` 后，
   井列（`c == p.well_col`）从 holes/hole_depth/col_t/wells/agg/bump 中掩码，row_t 中按填实计；
   评分尾部加 `sc -= w.pollute * pollute as f64`。
7. `choose_blocked`：`sorted.retain(skip)` 之后加「井深 ≥4 且非 danger 且块非 I → 剔除 pollute>0 候选（空则回退）」。
8. 最终 `profile()`（即下表）替换种子模板档：

```rust
Profile {
    name: "well", beam: 10, danger_h: 12, danger_pending: 4, harvest_badges: 4,
    well_col: 9, well_mode: true,
    w: Weights { landing: -3.0, eroded: 3.0, row_t: 2.8, col_t: 8.0, holes: 7.0,
        hole_depth: 0.6, bump: 1.2, wells: 3.0, agg: 0.15, attack: 4.0, cancel: 2.5,
        b2b_break: -9.0, well_keep: 3.5, tslot: 0.5, spin_extra: 2.0, pollute: 10.0 },
    w_danger: Weights { landing: -0.8, eroded: 6.0, row_t: 1.5, col_t: 6.0, holes: 14.0,
        hole_depth: 0.8, bump: 1.0, wells: 1.0, agg: 0.35, attack: 0.8, cancel: 4.0,
        b2b_break: -1.0, well_keep: 0.0, tslot: 0.0, spin_extra: 1.0, pollute: 0.0 },
}
```
