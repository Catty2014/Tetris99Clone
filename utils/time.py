#!/usr/bin/env python3
"""Arena Utils: time.py — 会话计时。

用法:
    python3 time.py [--json]

**第一次运行会记下会话开始时刻**：在当前工作目录写入 `.arena-session-start`。
此后每次运行都据它报告已用与剩余。删除该文件等于重置会话起点，操作员会审计这一点。

输出（JSON，--json 时为紧凑单行；否则为易读多行）:
    start               会话开始的墙钟时刻（epoch 秒）
    start_recorded      本次运行是否记下了起点（首次运行或显式重置时为 true）
    elapsed             自会话开始起的秒数
    remaining           距硬性截止的秒数
    recommended_remaining 距建议提交线的秒数
    deadline            硬性截止的绝对时刻（epoch 秒）

环境变量（由评测环境设置，优先于标记文件）:
    T99_SESSION_START       会话开始的 epoch 秒
    T99_SESSION_SECS        会话总时长秒（缺省 14400 = 4 小时，强制上限）
    T99_SESSION_RECOMMENDED 建议提交线秒（缺省 10800 = 3 小时）

自测示例:
    T99_SESSION_SECS=600 python3 utils/time.py
"""

import argparse
import json
import os
import sys
import time

MARKER = ".arena-session-start"


def load_start() -> tuple[float, bool]:
    env = os.environ.get("T99_SESSION_START")
    if env:
        return float(env), False
    if os.path.exists(MARKER):
        with open(MARKER, encoding="utf-8") as f:
            return float(f.read().strip()), False
    now = time.time()
    with open(MARKER, "w", encoding="utf-8") as f:
        f.write(f"{now:.3f}\n")
    return now, True


def main() -> int:
    ap = argparse.ArgumentParser(description="t99 会话计时")
    ap.add_argument("--json", action="store_true", help="输出紧凑 JSON")
    args = ap.parse_args()

    start, recorded = load_start()
    total = float(os.environ.get("T99_SESSION_SECS", "14400"))
    recommended = float(os.environ.get("T99_SESSION_RECOMMENDED", "10800"))
    now = time.time()
    deadline = start + total
    info = {
        "start": round(start, 3),
        "start_recorded": recorded,
        "elapsed": round(max(0.0, now - start), 3),
        "remaining": round(max(0.0, deadline - now), 3),
        "recommended_remaining": round(max(0.0, start + recommended - now), 3),
        "deadline": round(deadline, 3),
    }
    if args.json:
        print(json.dumps(info, separators=(",", ":")))
        return 0
    if recorded:
        print(f"会话开始已记录：{MARKER}")
    print(f"elapsed:               {info['elapsed']:.3f} s")
    print(f"remaining (hard 4h):   {info['remaining']:.3f} s")
    print(f"remaining (recommend): {info['recommended_remaining']:.3f} s")
    print(f"deadline:              {info['deadline']:.3f} (epoch)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
