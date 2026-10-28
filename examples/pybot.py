#!/usr/bin/env python3
"""t99 子进程示例程序：4 特征贪心（与内置 basic 同级），stdin/stdout 单行 JSON。

协议（v2）：
  收 {"type":"hello",...}            → 发 {"type":"ready"}
  收 {"type":"frame","n":N,...}      → 发 {"type":"input","n":N,"events":[{"t_us":0,"k":"cw"},...]}
  收 {"type":"result",...}           → 退出
"""

import json
import sys

PIECES = {
    "I": [(0, 1), (1, 1), (2, 1), (3, 1)],
    "O": [(0, 0), (1, 0), (0, 1), (1, 1)],
    "T": [(1, 0), (0, 1), (1, 1), (2, 1)],
    "S": [(1, 0), (2, 0), (0, 1), (1, 1)],
    "Z": [(0, 0), (1, 0), (1, 1), (2, 1)],
    "J": [(0, 0), (0, 1), (1, 1), (2, 1)],
    "L": [(2, 0), (0, 1), (1, 1), (2, 1)],
}
BOX = {"I": 4, "O": 2}


def cells(kind, rot):
    out = list(PIECES[kind])
    s = BOX.get(kind, 3)
    for _ in range(rot % 4):
        out = [(s - 1 - y, x) for (x, y) in out]
    return out


def eval_grid(g, cleared):
    heights = []
    holes = 0
    for c in range(10):
        top = 20
        for r in range(20):
            if g[r][c]:
                top = r
                break
        heights.append(20 - top)
        seen = False
        for r in range(20):
            if g[r][c]:
                seen = True
            elif seen:
                holes += 1
    agg = sum(heights)
    bump = sum(abs(heights[i] - heights[i + 1]) for i in range(9))
    return -0.510066 * agg - 0.35663 * holes - 0.184483 * bump + 0.760666 * cleared


def best_placement(board, kind):
    rots = 1 if kind == "O" else 4
    best = None
    for rot in range(rots):
        cs = cells(kind, rot)
        minx = min(x for x, _ in cs)
        maxx = max(x for x, _ in cs)
        for bx in range(-minx, 10 - maxx):
            y = 100
            for dx, dy in cs:
                col = bx + dx
                top = 20
                for r in range(20):
                    if board[r][col]:
                        top = r
                        break
                y = min(y, top - 1 - dy)
            if y < 0:
                continue
            g = [row[:] for row in board]
            for dx, dy in cs:
                g[y + dy][bx + dx] = 1
            cleared = sum(1 for row in g if all(row))
            if cleared:
                g = [row for row in g if not all(row)]
                g = [[0] * 10 for _ in range(cleared)] + g
            sc = eval_grid(g, cleared)
            if best is None or sc > best[0]:
                best = (sc, rot, bx)
    return None if best is None else (best[1], best[2])


def parse_board(rows):
    return [[0 if ch == "." else 1 for ch in row] for row in rows]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    plan_key = None
    plan = None
    sent_target = False
    while True:
        line = sys.stdin.readline()
        if not line:
            return
        msg = json.loads(line)
        t = msg.get("type")
        if t == "hello":
            send({"type": "ready"})
        elif t == "result":
            return
        elif t == "frame":
            n = msg["n"]
            if not sent_target:
                sent_target = True
                send({"type": "input", "n": n, "events": [{"t_us": 0, "k": "target_random"}]})
                continue
            a = msg.get("active")
            if a is None:
                send({"type": "input", "n": n, "events": []})
                continue
            board = parse_board(msg["board"])
            key = (msg["pieces"], a["t"])
            if key != plan_key:
                plan_key = key
                plan = best_placement(board, a["t"])
            if plan is None:
                send({"type": "input", "n": n, "events": []})
                continue
            trot, tcol = plan
            events = []
            diff = (trot - a["r"]) % 4
            if diff == 3:
                events.append((0, "ccw"))
            elif diff != 0:
                events.append((0, "cw"))
            elif a["x"] < tcol:
                events = [(0, "right_down"), (4000, "right_up")]
            elif a["x"] > tcol:
                events = [(0, "left_down"), (4000, "left_up")]
            else:
                events = [(0, "hard")]
            send({"type": "input", "n": n, "events": [{"t_us": t, "k": k} for t, k in events]})


if __name__ == "__main__":
    main()
