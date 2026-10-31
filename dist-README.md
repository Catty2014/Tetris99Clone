# Tetris99Clone — 分发目录（dist/）

对局模块的完整分发：静态二进制（musl，无运行时依赖）+ 可独立构建的源码。
不含未公开档位（master / grandmaster）的任何源码或二进制。

## 组件

| 组件 | 形式 | 说明 |
| --- | --- | --- |
| t99-core | 源码 + 二进制（headless 内含） | 确定性引擎（内核） |
| t99-proto | 源码 + 文档（`docs/` 与题目 `protocol.md`） | 线协议 v8 + `snapshot_from_frame` 反向构造 |
| t99-headless | 源码 + `bin/t99-headless` | 无头主机：run / replay / calibrate；内含全部内建档位（expert 为闭源内建） |
| t99-server | 源码 + `bin/t99-server` | 单局权威服务器（stdio 子进程 + WebSocket 玩家/观战） |
| t99-bots | 源码（无 expert；`--no-default-features` 可构建） | 内建 basic / advanced / null 源码；expert 源码不提供 |
| t99-bot-ext | 源码 + `bin/t99-bot-basic` `bin/t99-bot-advanced` | 公开档外部程序（参考实现/可修改陪练） |
| master / grandmaster | 仅存在说明 | 源码与二进制不提供，只能在对局中遇到 |
| examples/pybot.py | 源码 | Python 子进程示例（跨语言协议参考） |
| utils/time.py | 源码 | Arena Utils：会话墙钟查询 |
| ruleset.toml | 配置 | 规则集（与二进制编译期 RULESET_ID 绑定，修改需重新派生） |
| server.toml / client.toml | 配置 | 服务器/客户端参考配置 |

## 快速开始

```bash
# 本地无头对局（内建档位 + 外部程序混编）
./bin/t99-headless run --players 4 --seed 42 \
    --bots "basic,ext:basic=./bin/t99-bot-basic,advanced"

# 单局服务器 + lobby（等待玩家接入）
./bin/t99-server --port 8138 --key t99 --players 4 --lobby \
    --bots "ext:basic=./bin/t99-bot-basic,advanced"

# 你的程序以子进程接入（stdin/stdout 单行 JSON，见 protocol.md）
# 或以 WebSocket 玩家接入：
./bin/t99-bot-advanced --connect ws://127.0.0.1:8138 --id my-advanced --key t99

# 逐位回放校验（确定性）
./bin/t99-headless replay --transcript /tmp/m.jsonl

# 分发源码独立构建（不含 expert；`cargo test` 同样可用）
cd src && cargo build --release
```

## 注意

- 二进制为 musl 静态链接（x86_64），无 libc/运行时依赖。
- 分发源码中 `crates/t99-bots` 的 expert 档位已剔除（feature 关闭）；
  分发二进制中的 expert 为闭源内建实现，仅可通过内建档位名 `expert` 使用。
- 引擎语义版本（`ENGINE_SEMANTICS`）与协议号随握手下发，两端不一致会被拒绝。
