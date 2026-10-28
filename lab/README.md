# lab/ — 策略 bot 实验场

每个子目录是一个**独立 cargo 工作区**的实验 bot（外部程序，与参赛模型提交同线协议）。
主仓库 `crates/` 一概不动。选秀后优胜策略移植回 `crates/t99-bot-m`（ADR-026）。

## 规则

1. 沙盒：只允许写自己 `lab/<name>/` 目录；仓库其余路径只读。
2. 依赖仅 serde / serde_json（保证参赛线协议自洽）；行为确定（禁墙钟/随机）。
3. `decide()` 要快：常规 <3ms，硬上限 16ms（服务器帧预算）。
4. 对局测试**串行**跑（并行 duel 会因 CPU 争用扰动子进程 16ms 应答，产生假 miss）。
5. 调试旋钮（种子代码自带）：`T99_BOT_DEBUG=1`（每块决策 top3）、`T99_BOT_TRACE=1`（逐帧事件）。

## 测试工作台（已预构建，直接用）

```
/home/pig2014/Lab/NPB/t99/target/release/examples/duel \
  --players 5 --matches 8 --time-limit-secs 120 --seed 42 \
  --bots "ext:you=<你的二进制>,ext:m=/home/pig2014/Lab/NPB/t99/target/release/t99-bot-master,expert,advanced,basic"
```

- 输出：每局名次 + 汇总（平均名次 / 行块 / 共送出 / PPS）。
- 基线（当前正式档）：`t99-bot-master`（5p FFA 平均名次 ≈2.0、送出 ≈90/局）、
  `t99-bot-gm`（≈1.0、送出 ≈100/局）；内建 expert/advanced/basic。
- 校准路径：先 solo 存活（`--players 1`，4 局 150s 全满时）→ 进攻（FFA 送出量）→ 名次。
- 席位语法 `ext:label=path`；label 同场唯一（统计按 label 聚合）。

## 收尾交付

- `lab/<name>/REPORT.md`：策略说明、试过什么、与基线对照数字、结论。
- REPORT 末尾附「移植」一节：最终 Profile 字段值 + 新增策略函数的精确位置，便于并回主仓库。
