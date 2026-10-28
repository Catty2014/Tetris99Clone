# lab/berserk — KO 猎杀 / 极限激进策略实验报告

**定位**：「可以不活，其余参赛者必须死」——比现任 GM（attack 5.0 / 危险 16 行 / 垃圾容忍 8）
更极端的 KO 猎杀档位，用实测回答：极限激进到底划不划算。

**二进制**：`/home/pig2014/Lab/NPB/t99/lab/berserk/target/release/lab-berserk`
（选档环境变量 `T99_BERSERK=v1..v6`，缺省 = 实测最优 v7）

---

## 1. 策略设计

种子 = `lab/_template`（Master/GM 大脑）。改动全部在 `src/lib.rs`：

1. **Berserk 谱系**（`fn berserk()` 骨架 + v1–v7 构造器）：在 GM 基础上
   - `w.attack` 5→8/12/16/20（v1/v2/v5/v7）；`danger_h` 16→18/20；`danger_pending` 8→10/12/14/16；
   - 静态权重整体向进攻倾斜：landing 罚 -4.0→-3.4、holes 6→5/4/3.5、eroded 3→2.6/2.2/2.0、
     `spin_extra` 4→5、`cancel` 1.6→1.2（v6 对照 2.5）；
   - **兜底层原样保留 GM 的 `w_danger`**（挖行 6.5 / 抵消 4.5 / 攻击 0.6）——「激进到底但不是自杀」。
2. **目标策略**（`Brain::target_mode`）：被瞄 → `TargetAttackers`；徽章 ≥1 → `TargetBadges`
   （去掉 GM 的 b2b/combo 门槛）；否则永远 `TargetKos`。
3. **KO 钓鱼（假死）**：`Profile.feign/feign_cap` + `danger_state()`：被瞄准且堆高 <21 行时
   **维持攻势权重**（消行=抵消+反打双赚），生存红线抬到 `feign_cap`；v3=开钓鱼，v4=同参数关钓鱼（对照）。
4. 线协议 / 引擎复刻 / 候选生成 / (x,rot) BFS 执行器**未动**（键型/t_us 约束由原 `pack()`/执行器保证）。

## 2. 测量环境的重要校正（影响所有名次解读）

- 5p/10p 强场在 120s 时限内**几乎无人死亡**（GM/berserk/master/expert 互相攻不破：
  5p 300s、600s 均 0 击杀；10p 约 **1.0 首杀/局**）。全员存活到时限时，引擎流局 tie-break
  =（徽章 desc, pid asc）→ **平均名次≈座位序伪影**。
- 因此本文主指标=**共送出**（攻击力，不受 tie-break 污染）+ solo 存活 + 10p 场（有真实击杀、
  名次部分由徽章决定）的名次；5p 强场名次仅作占位。每组标注击杀情况。

## 3. solo 存活（--players 1，4 局 ×150s，seed 7）

| 档位 | 满时存活 | 备注 |
|---|---|---|
| v1 (attack8/18行/10垃圾) | **4/4** | PPS 3.76 |
| v2 (attack12/20行/12垃圾) | **4/4** | PPS 3.75 |
| v3 (=v2+钓鱼) | **4/4** | 同 v2 |
| **v7 (attack20/20行/16垃圾)** | **4/4** | 454 块/局，seed99 复验 1/1 |

极端档 v7 也能全时存活：GM 强兜底层保住了「不自杀」底线。

## 4. 激进度扫描 — 5p 强场（berserk+gm+expert+advanced+basic，120s×8 局×2 seeds，双向座位；0 击杀，名次=座位伪影，看送出）

| 档位 | attack / 危险行 / 垃圾容忍 | 送出均值（4 段） | 同场 GM 送出 | Δ |
|---|---|---|---|---|
| v1 | 8 / 18 / 10 | **99.3** | 91.3 | +8.8% |
| v2 | 12 / 20 / 12 | **101.0** | 91.7 | +10.1% |
| v5 | 16 / 20 / 14 | **106.1** | 94.4 | +12.4% |
| v6（v2+cancel2.5 对照） | 12 / 20 / 12 | 99.3 | — | cancel 回升反而 -1.7 送出 |

vs master（v2，同协议）：berserk 104.8 vs master 97.7（+7.3%）。

## 5. 10p 大场（berserk+gm+master+7 内建档，120s×8 局；≈1.0 真实首杀/局，名次含徽章成分）

| 档位 | 座位 | seed | 平均名次 | 送出 | 同场 gm |
|---|---|---|---|---|---|
| v1 | 0 | 42/1337 | 1.62 / 1.00 | 125.4 / 116.5 | — |
| v2 | 0 | 42/1337 | 1.62 / 1.00 | 129.9 / 122.0 | — |
| v5 | 0 | 42/1337 | **1.38 / 1.25** | 150.5 / 123.1 | — |
| **v7** | 0 | 42/1337 | 1.50 / **1.12** | **151.9 / 143.8** | — |
| v7 | 1（对称位） | 42/1337 | 2.62 / 2.12 | **177.6 / 168.2** | 座位 0（名次占优为座位伪影，送出 v7 约为 gm 2 倍） |
| gm 参照（无 berserk 场） | 0 | 42 | 1.62 | 107.8 | — |
| v7 缺省档复验 seed99 | 0 | 99 | **1.00（2 局，含真实击杀局）** | 139/104 | gm 113/98，m 140/92 |

**综合（平均名次 × 送出）最优 = v7**：送出比现任 GM 高 **+35%~70%**（视字段/座位），
名次持平或更好（1.12–1.50 vs gm 参照 1.62），且 0 死亡、0 program_dead。

## 6. KO 钓鱼（假死）实验结论：**无净收益证据，也无代价**

v3（钓鱼开）与 v2（关）、v4（同参数对照）在 5p 与 10p 全部字段**逐数字完全相同**
（如 10p s42：三者都是 rk 1.62 / sent 129.9）。原因：触发条件=「被瞄准 且 堆高≥20 行」，
实测中 v2/v3 的堆高几乎从不越过 20 行红线（攻击型打法堆不起来），钓鱼态**从未进入**。
即：在当前引擎垃圾压力下，KO 钓鱼既不赚也不亏——它防御的困境不会发生。若未来字段
火力翻倍，可复验（代码保留，`T99_BERSERK=v3`）。

## 7. 迭代记录（轮次摘要）

1. v1（attack8）：5p 送出 +8.8% vs gm，solo 4/4 → 激进有正收益。
2. v2（attack12）：+10.1%，solo 4/4 → 继续加码。
3. v3/v4 钓鱼 A/B：零差异 → 钓鱼判死（触发不了）。
4. v5（attack16/tolerate14）：+12.4%，10p 名次最好（1.38/1.25）。
5. v6（cancel 回升 2.5）：送出回落 → **高攻档不要回补抵消权重**。
6. v7（attack20/tolerate16）：10p 送出 143.8–177.6，solo 4/4，seed99 复验 2/2 第一 → **定稿缺省档**。

## 8. 结论：是否推荐移植

**推荐移植 v7 档**——「现任 GM 已是激进最优点」不成立：在 GM 强兜底层不动的前提下，
attack 5→20、危险阈值 16→20 行、垃圾容忍 8→16 的一路加码，送出单调上升
（5p：+10~12%，10p：+35~70%），名次不降反升，solo 仍 4/4 满时存活。
激进的上限还没探到亏损区（v7 已是 attack 20；v6 证明唯一不能动的是 cancel/兜底层）。

**移植清单**（目标 `crates/t99-bot-m`，ADR-026）：
- `Profile` 增加 `feign: bool, feign_cap: usize`（可裁剪：钓鱼已证无效，直接不带）；
- 新增 `fn berserk(...)` + `pub fn berserk7()`（`src/lib.rs`，紧随 `gm_profile()` 之后），
  字段值 = gm_profile() 改：`danger_h=20, danger_pending=16, harvest_badges=1`，
  `w = { landing:-3.4, eroded:2.0, row_t:2.4, col_t:7.0, holes:3.5, wells:2.0, attack:20.0,
  cancel:1.2, b2b_break:-6.0, tslot:1.0, spin_extra:5.0, 其余 0 }`，`w_danger` = GM 原值不动；
- `danger_state()`（钓鱼版可简化回原布尔式）、`target_mode()` 去掉徽章收割的 b2b/combo 门槛；
- `candidates/push_cand/evaluate` 增加 `redline_h` 参数（钓鱼红线，可裁剪）。

## 附：复现

```bash
D=/home/pig2014/Lab/NPB/t99/target/release/examples/duel
B=/home/pig2014/Lab/NPB/t99/lab/berserk/target/release/lab-berserk
G=/home/pig2014/Lab/NPB/t99/target/release/t99-bot-gm
M=/home/pig2014/Lab/NPB/t99/target/release/t99-bot-master
# solo
$D --players 1 --matches 4 --time-limit-secs 150 --seed 7 --bots "ext:x=$B"
# 10p 大场（真实击杀）
$D --players 10 --matches 8 --time-limit-secs 120 --seed 42 \
  --bots "ext:x=$B,ext:gm=$G,ext:m=$M,expert,advanced,basic,expert,advanced,basic,basic"
# 选档：T99_BERSERK=v1..v6（缺省 v7）；调试 T99_BOT_DEBUG=1 / T99_BOT_TRACE=1
```
