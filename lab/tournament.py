#!/usr/bin/env python3
"""选秀锦标赛 v2（击杀导向）。

背景：强 bot 互打的抵消≈火力，5p/1v1 常 0 击杀撑满时限，名次沦为座位序伪影
（combo 代理实验确认）。真实区分度来自：让弱档溺死的 10 人混战场（KO/徽章）、
以及纯火力压制指标（1v1 送出差）。因此本锦标赛只认三类硬指标：
  A. murder-ball（10p）：候选×3 + master×2 + gm×2 + expert×2 + advanced + basic
     —— 弱档被淹死，KO/徽章/名次才有意义
  B. 1v1 vs master / gm：火力差（送出）+ 双方存活（压制哨兵）
  C. solo 存活（ sanity，选手自报）

用法：
  python3 lab/tournament.py --quick            # 每组 6 局（粗筛）
  python3 lab/tournament.py --matches 12       # 决赛
  python3 lab/tournament.py --candidates a=path b=path
"""

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass

DUEL = "/home/pig2014/Lab/NPB/t99/target/release/examples/duel"
M = "/home/pig2014/Lab/NPB/t99/target/release/t99-bot-master"
GM = "/home/pig2014/Lab/NPB/t99/target/release/t99-bot-gm"

CANDIDATES = {
    # label: 二进制路径（选秀时填入）
    "well": "/home/pig2014/Lab/NPB/t99/lab/well/target/release/lab-well",
    # "tspin": "/home/pig2014/Lab/NPB/t99/lab/tspin/target/release/lab-tspin",
    # "combo": "/home/pig2014/Lab/NPB/t99/lab/combo/target/release/lab-combo",
    # "adaptive": "/home/pig2014/Lab/NPB/t99/lab/adaptive/target/release/lab-adaptive",
    # "berserk": "/home/pig2014/Lab/NPB/t99/lab/berserk/target/release/lab-berserk",
}

@dataclass
class Row:
    label: str
    place: float
    lp: float
    sent: float
    pps: float
    surv: float
    ko: float

SUMMARY_RE = re.compile(r"^\s+(\S+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*$")


def run_duel(bots: list[str], matches: int, seed: int, secs: int = 150) -> list[Row]:
    cmd = [DUEL, "--players", str(len(bots)), "--bots", ",".join(bots),
           "--matches", str(matches), "--time-limit-secs", str(secs), "--seed", str(seed)]
    print(f"$ {' '.join(cmd)}", flush=True)
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=7200)
    rows, in_summary = [], False
    for line in out.stdout.splitlines():
        if line.startswith("  席位"):
            in_summary = True
            continue
        if line.startswith("==="):
            in_summary = False
        m = SUMMARY_RE.match(line)
        if in_summary and m:
            rows.append(Row(m.group(1), *(float(x) for x in m.groups()[1:])))
    if not rows:
        print(out.stdout[-2000:], out.stderr[-1500:], file=sys.stderr)
        raise RuntimeError("duel 汇总解析失败")
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--matches", type=int, default=12)
    ap.add_argument("--candidates", nargs="*", help="label=path ...")
    args = ap.parse_args()
    n = 6 if args.quick else args.matches
    cands = dict(CANDIDATES)
    for spec in args.candidates or []:
        k, v = spec.split("=", 1)
        cands[k] = v
    if not cands:
        sys.exit("候选为空")

    print(f"# 选秀锦标赛 v2：{len(cands)} 候选 × 每组 {n} 局\n")
    murder_ball = ["expert", "expert", "advanced", "basic"]  # 弱档溺死池
    records = {c: {"place": [], "ko": [], "sent": [], "surv": [], "pm": [], "pg": [], "sm": [], "sg": []} for c in cands}

    for label, path in cands.items():
        you = [f"ext:{label}1={path}", f"ext:{label}2={path}", f"ext:{label}3={path}"]
        field = you + [f"ext:m={M}", f"ext:m2={M}", f"ext:g={GM}", f"ext:g2={GM}"] + murder_ball
        for seed in (4242, 90210):
            rows = run_duel(field, n, seed=seed)
            r = next(r for r in rows if r.label == f"ext:{label}1")
            records[label]["place"].append(r.place)
            records[label]["ko"].append(r.ko)
            records[label]["sent"].append(r.sent)
            records[label]["surv"].append(r.surv)
            weak = [r for r in rows if r.label in ("expert", "advanced", "basic")]
            weak_deaths = sum(1.0 - x.surv for x in weak) / len(weak)
            print(f"  A[{label} s{seed}]: 名次{r.place:.2f} KO/局{r.ko:.2f} 送出{r.sent:.0f} 存活{r.surv:.2f} | 弱档死亡率{weak_deaths:.2f}", flush=True)
        for foe_name, foe, pk, sk in [("M", [f"ext:m={M}"], "pm", "sm"), ("GM", [f"ext:g={GM}"], "pg", "sg")]:
            rows = run_duel(you[:1] + foe, n, seed=8000 + len(label))
            mine = next(r for r in rows if r.label == f"ext:{label}1")
            theirs = next(r for r in rows if r.label.startswith("ext:m") or r.label.startswith("ext:g"))
            records[label][pk].append(theirs.place)
            records[label][sk].append(mine.sent - theirs.sent)
            print(f"  B[1v1 vs {foe_name}]: 火力差 {mine.sent - theirs.sent:+.0f}（我 {mine.sent:.0f} / 对 {theirs.sent:.0f}）双存 {mine.surv:.2f}/{theirs.surv:.2f}", flush=True)

    print("\n# ===== 决算（A 场为击杀导向主指标）=====")
    print(f"{'候选':<12} {'A名次':>6} {'A_KO/局':>8} {'A送出':>7} {'A存活':>6} {'1v1火力差M':>10} {'1v1火力差GM':>11}")
    ranked = sorted(records.items(), key=lambda kv: (sum(kv[1]["place"]) / len(kv[1]["place"]), -sum(kv[1]["ko"])))
    for label, rec in ranked:
        avg = lambda k: sum(rec[k]) / len(rec[k])
        print(f"{label:<12} {avg('place'):>6.2f} {avg('ko'):>8.2f} {avg('sent'):>7.0f} {avg('surv'):>6.2f} {avg('sm'):>+10.0f} {avg('sg'):>+11.0f}")
    print("\n解读：A 场名次/KO 在有死亡的场次才有意义（弱档死亡率>0 时）；1v1 火力差=纯压制力。")


if __name__ == "__main__":
    main()
