#!/bin/bash
# 双座位对称扫描：每次 run 输出一行「label rk(label) sent(label) rk(对照) sent(对照)」
D=/home/pig2014/Lab/NPB/t99/target/release/examples/duel
B=/home/pig2014/Lab/NPB/t99/lab/berserk/target/release/lab-berserk
G=/home/pig2014/Lab/NPB/t99/target/release/t99-bot-gm
M=/home/pig2014/Lab/NPB/t99/target/release/t99-bot-master

summarize() { # stdin=duel stdout, $1=labelA $2=labelB(可空)
  awk -v A="$1" -v B="$2" '
    /汇总/ {insum=1; next}
    insum && /^  / {
      seat=$1; rk=$2; sent=$4
      if (seat=="ext:"A) {ra=rk; sa=sent}
      if (B!="" && seat=="ext:"B) {rb=rk; sb=sent}
    }
    END {
      if (B!="") printf "%s rk=%.2f sent=%.1f | %s rk=%.2f sent=%.1f", A, ra, sa, B, rb, sb
      else printf "%s rk=%.2f sent=%.1f", A, ra, sa
    }'
}

run() { # $1=desc $2=env $3=bots
  local desc="$1" envv="$2" bots="$3"
  printf '%-40s ' "$desc"
  for s in 42 1337; do
    out=$(env $envv $D --players 5 --matches 8 --time-limit-secs 120 --seed $s --bots "$bots" 2>/dev/null)
    printf 's%d[%s] ' $s "$(echo "$out" | summarize "${LABEL_A}" "${LABEL_B}")"
  done
  echo
}

# ———— R2: 强场（+expert/advanced/basic）双向座位 ————
LABEL_A=berserk LABEL_B=gm
run "R2 v1 seat0 vs gm  s42/1337" "T99_BERSERK=v1" "ext:berserk=$B,ext:gm=$G,expert,advanced,basic"
run "R2 v1 seat1 vs gm  s42/1337" "T99_BERSERK=v1" "ext:gm=$G,ext:berserk=$B,expert,advanced,basic"
run "R2 v2 seat0 vs gm  s42/1337" "T99_BERSERK=v2" "ext:berserk=$B,ext:gm=$G,expert,advanced,basic"
run "R2 v2 seat1 vs gm  s42/1337" "T99_BERSERK=v2" "ext:gm=$G,ext:berserk=$B,expert,advanced,basic"
LABEL_A=berserk LABEL_B=m
run "R2 v1 seat0 vs master" "T99_BERSERK=v1" "ext:berserk=$B,ext:m=$M,expert,advanced,basic"
run "R2 v2 seat0 vs master" "T99_BERSERK=v2" "ext:berserk=$B,ext:m=$M,expert,advanced,basic"
run "R2 v2 seat1 vs master" "T99_BERSERK=v2" "ext:m=$M,ext:berserk=$B,expert,advanced,basic"

# ———— R3: KO 可致死场（berserk/gm + 4 basic）———
LABEL_A=berserk LABEL_B=""
run "R3 v2 seat0 +4basic" "T99_BERSERK=v2" "ext:berserk=$B,basic,basic,basic,basic"
run "R3 v2 seat4 +4basic" "T99_BERSERK=v2" "basic,basic,basic,basic,ext:berserk=$B"
run "R3 v1 seat0 +4basic" "T99_BERSERK=v1" "ext:berserk=$B,basic,basic,basic,basic"
run "R3 v1 seat4 +4basic" "T99_BERSERK=v1" "basic,basic,basic,basic,ext:berserk=$B"
LABEL_A=gm LABEL_B=""
run "R3 gm  seat0 +4basic" "" "ext:gm=$G,basic,basic,basic,basic"
run "R3 gm  seat4 +4basic" "" "basic,basic,basic,basic,ext:gm=$G"
