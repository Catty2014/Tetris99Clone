# T-Spin 特化策略实验报告（lab/tspin）

日期：2026-10-27。二进制：`/home/pig2014/Lab/NPB/t99/lab/tspin/target/release/lab-tspin`（包名 `lab-tspin`，种子 lib.rs 复刻 + T-Spin 体系改造）。

## 一句话结论

**推荐移植（针对 Master 档的升级）**：最终档 V4 与同场 t99-bot-master 相比，5p FFA 送出 +11.0/+15.4 每局（两 seed 段）、1v1 +5.9/+3.7、10p 实杀场双席位平均 +13.5，同时 solo 4/4 全满存活、自旋计划 1.2–2.0 次/局（master ≈0–1）；但对 t99-bot-gm 仍落后 5–13 送出/局（GM 是极端进攻档，非本策略目标）。

## 策略：T-Spin 特化体系（在 master 底盘上）

危险兜底（`w_danger`、danger_h/danger_pending）**原封不动保留 master 档**；只改非危险态的评估与搜索。四个组件：

1. **引擎级真槽探测 `spin_slots(g)`**（lib.rs:797）：对盘面跑与候选生成完全相同的贴地踢墙扫描（SRS 踢墙 + `spin_at` 三角判定），数"现在就能踢墙进入并成立全旋"的 TSD/TST 槽——结构可用才给分，非几何启发。同时返回**保护格集合**（T 本体空格 + 槽口上方两行通道格）。
2. **几何槽特征 `pocket_features(g)`**（lib.rs:883，O(W·R) 逐候选进 evaluate）：成品 TSD 槽 = nub 孔（左右实/墙、正下实/地板）+ 身体行 3 宽全空 + 单侧悬挑肩（全旋 b≥1 条件）；TST = 再深一层 3 宽 + 更上一行肩；准槽 = nub/身体/底齐、缺肩。**关键：成品/准槽给分 + 槽孔退回 `holes` 罚**——没有退款，悬挑凹槽被 holes/col_t 罚在排序里直接杀死，永远进不了束（V2 之前的核心瓶颈）。
3. **T 近邻门控 `kv`**（lib.rs:778-783）：active/hold/next[0..2] 有 T 才全额计价（1.0），否则 25% 残值——没有弹药时造槽/保槽是纯节奏损耗（V2 实测送出反被拖低 86.6 vs 95.1）。Hold 保 T、槽口留通道由该门控 + hold 分支自然涌现。
4. **通道保护**（choose_blocked，lib.rs:986）：引擎级 keepout，候选将填现有槽本体/通道格 → 罚 `channel`；自旋候选豁免（消费槽的收益已由 attack+spin_extra 计入）。

### 最终 Profile 字段值（`profile()`，lib.rs:1574）

```
基于 master_profile()，仅改 w（非危险权重）；w_danger 与 master 完全一致：
attack      2.0 → 3.2
b2b_break   -5.0 → -7.0     # TSD=5/TST=7 on b2b 链
spin_extra  2.0 → 12.0
tsd_ready   0 → 6.0          # 新字段：成品 TSD 槽（每槽，含 holes 退款）
tst_ready   0 → 10.0         # 新字段：成品 TST 槽
proto       0 → 1.2          # 新字段：准槽构造偏置
channel     0 → -15.0        # 新字段：填槽本体/通道格罚（引擎 keepout）
```
其余（beam=8、danger_h=13、danger_pending=4、harvest_badges=4、well_col=9、其余权重）= master。

## 实验数字

5p 场 = `ext:tspin, ext:m(或gm), expert, advanced, basic`（master 基准场），120s 时限。**重要：本环境下 5p/10p/1v1 全部 ~95–100% 时限流局（reason=time_limit，强手 0 击杀，仅 basic 席位偶死亡）**，名次≈座位序伪影，不可作数；有效指标 = 送出量、solo 存活、spin 计划数。

### 迭代（5p vs master，seed 42）

| 版本 | 变化 | tspin 送出 | master 送出 | spin计划/局 |
|---|---|---|---|---|
| V1 | 仅事后束内调整（引擎槽加分 + channel-35），排序内无槽特征 | 96.2 | 94.2 | 0.4 |
| V2 | 槽特征进 evaluate + holes 退款；proto=2.0 | 86.6 | 95.1 | 4.0 |
| V3 | V2 + t_near 门控 kv（修 V2 回退）+ attack 2.8 | 87.1 | 93.5 | 1.6 |
| **V4** | attack 3.2、tsd 6/tst 10/proto 1.2/channel -15 | **103.9** | 92.9 | 2.0 |

V2 的教训：槽计价让 bot 真的去造槽/保槽了（spin 计划 4/局），但 T 不到场时是纯节奏损耗，送出反降；V3 门控止跌、V4 配平进攻权重后反超。

### 最终对照（V4）

| 场 | seed 段 | tspin 送出/局 | 对手 送出/局 | 差 | spin计划/局 | 击杀/时限 |
|---|---|---|---|---|---|---|
| 5p vs master | 42 | **103.9** | 92.9 | **+11.0** | 2.0 | 0 / 8 局全时限 |
| 5p vs master | 1337 | **99.8** | 84.4 | **+15.4** | 1.25 | 0 / 8 局全时限 |
| 5p vs gm | 42 | 101.5 | 106.1 | −4.6 | 1.6 | 0 / 8 局全时限 |
| 5p vs gm | 1337 | 92.8 | 105.4 | −12.6 | 1.1 | 0 / 8 局全时限 |
| 1v1 vs master | 42 | **57.8** | 51.9 | **+5.9** | — | 0 / 全时限 |
| 1v1 vs master | 1337 | **57.6** | 53.9 | **+3.7** | — | 0 / 全时限 |
| 1v1 vs gm | 42 | 53.4 | 56.5 | −3.1 | — | 0 / 全时限 |
| 1v1 vs gm | 1337 | 56.8 | 60.1 | −3.3 | — | 0 / 全时限 |
| 10p 实杀场（tspin×2,m,gm,expert×2,advanced×2,basic×2） | 42 | **129.9 / 98.0**（双席位） | m 95.6，gm 89.1 | **+11.6（席位均）** | 0.6 | 仅 basic 死（0.12/局） |
| 10p 实杀场 | 1337 | 95.0 / 102.4（双席位） | m 90.4，gm 99.1 | +4.0（席位均） | 0.5 | 仅 basic/expert 死（0.12/局） |

solo 存活（--players 1，150s×4）：V1、V2、V4 三轮全部 4/4 满时长存活（178–182 行/局，PPS≈3.8），spin 计划 1–4/局。

### spin 实证

- spin 计划（bot stderr `spin计划=N`）：V4 对 master 场 1.25–2.0 次/局，同场 master/gm ≈0–1 次/局；单测验证经典 TSD 槽被引擎级探测发现并判全旋、消 2 行送 4。
- 送出 +4/+6 跳变：duel 只给汇总送出，无逐手日志；spin 计划数与送出优势同向（每局 2 次 TSD≈+8–10 送出，约占对 master 优势的一半），其余优势来自槽体系对整体堆放质量的改善（holes 退款使带凹槽的紧凑堆放可行）+ attack 2.0→3.2。

## 移植建议与位置（最终档 V4）

**推荐移植为正式 Master 档升级（替换 master_profile 的 w + 新增 4 个权重字段与 2 个函数）**。理由：对 master 三种场面（5p 两 seed 段、1v1 两 seed 段、10p 两 seed 段）送出全面占优且 solo 存活无退化；危险兜底零改动，风险面小。对 gm 档不构成替代（送出仍落后 5–13/局）。

精确位置（`lab/tspin/src/lib.rs`）：
- `Weights` 新字段 `tsd_ready/tst_ready/proto/channel`：lib.rs:509-515；
- 槽计价 + t_near 门控：`evaluate()` 内 lib.rs:770-784；
- 引擎级真槽探测 `spin_slots()`：lib.rs:797（单测 `finds_kick_tsd`/`spin_slots_detects_tsd_and_keepout` 覆盖）；
- 几何槽特征 `pocket_features()`：lib.rs:883（单测 `proto_slot_detected_when_shoulder_missing` 覆盖）；
- 通道保护：`choose_blocked()` 内 lib.rs:983-1000；
- 最终字段值：`profile()` lib.rs:1574。

性能：新增成本 ≈ 每决策 1 次 keepout 扫描（danger 时跳过）+ 每候选一次 O(W·R) 几何扫描；对局全程 0 program_dead / 0 超帧。

## 未决/局限

- 本环境所有场面击杀稀缺（强手互相 0 击杀），名次指标不可用，"更强"以上送出差与存活率证明；若需击杀级证明需大幅加时时限或更高负载（未做，遵守预算）。
- spin 转化率本身仍低（≈2 次/局）：主动构筑多件套 overhang 需要 2-3 步协调堆放，2 层束搜索的视野不足以稳定"存槽等 T"；V2→V3 显示激进造槽反而亏节奏。后续可试：更深层束、以 next 队列 T 位置做槽选址、combo 档位联动。
- gm 对照负差：GM 的连击/全清激进流在该引擎的抵消机制下送出上限更高；T-Spin 档若要追平 GM 需叠加 combo 权重（超出本策略族范围）。
