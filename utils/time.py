#!/usr/bin/env python3
"""Arena Utils: time.py — 会话墙钟查询（monotonic，不受系统时间调整影响）。

用法:
    python3 time.py [--json]

输出（JSON，--json 时为紧凑单行；否则为易读多行）:
    elapsed    自会话开始起的秒数
    remaining  距硬性截止的秒数
    deadline   硬性截止的绝对时刻（monotonic 秒）

环境变量（由评测环境/自测脚本设置）:
    T99_SESSION_START  会话开始的 monotonic 秒（缺省 0）
    T99_SESSION_SECS   会话总时长秒（缺省 5400 = 90 分钟）

自测示例:
    T99_SESSION_START=$(python3 -c 'import time;print(time.monotonic())') \
    T99_SESSION_SECS=600 python3 utils/time.py
"""

import argparse
import json
import os
import sys
import time


def main() -> int:
    ap = argparse.ArgumentParser(description="t99 会话墙钟查询")
    ap.add_argument("--json", action="store_true", help="输出紧凑 JSON")
    args = ap.parse_args()

    start = float(os.environ.get("T99_SESSION_START", "0"))
    total = float(os.environ.get("T99_SESSION_SECS", "5400"))
    now = time.monotonic()
    deadline = start + total
    info = {
        "elapsed": round(max(0.0, now - start), 3),
        "remaining": round(max(0.0, deadline - now), 3),
        "deadline": round(deadline, 3),
    }
    if args.json:
        print(json.dumps(info, separators=(",", ":")))
    else:
        print(f"elapsed:   {info['elapsed']:.3f} s")
        print(f"remaining: {info['remaining']:.3f} s")
        print(f"deadline:  {info['deadline']:.3f} (monotonic)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
