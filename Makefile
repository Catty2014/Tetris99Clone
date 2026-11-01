# Tetris99Clone — 分发构建
#
# make check   全 workspace 测试 + ext basic 冒烟对局
# make dist    musl 静态构建 + 组装 dist/（剔除断言 + dist/src 自洽构建检查）
# make clean   清理 dist/ 与 musl 产物

MUSL      := x86_64-unknown-linux-musl
MUSL_BIN  := target/$(MUSL)/release
HOST_BIN  := target/release
DIST      := dist
# 题目目录（AX1）：make dist 时自动把分发模块部署进去，并同步公开 bot 源码
AX1_DIR   ?= ../Problem/AX1

# 分发二进制（musl）
DIST_BINS := t99-server t99-headless t99-bot-basic t99-bot-advanced
# 分发源码 crate（完整 Arena Module；剔除 t99-client / t99-bot-m）
SRC_CRATES := t99-core t99-proto t99-config t99-bots t99-headless t99-server t99-bot-ext

.PHONY: all check smoke musl-build dist verify-dist clean

all: check

check: smoke
	cargo test --workspace

# 冒烟：外部程序与内建 bot 同局（不同通道、同一物理）
smoke:
	cargo build --release -p t99-headless -p t99-bot-ext
	./$(HOST_BIN)/t99-headless run --players 4 --seed 7 --max-frames 36000 \
	    --bots "ext:basic=./$(HOST_BIN)/t99-bot-basic,advanced,basic,null" >/dev/null
	@echo "[smoke] ext basic 完成对局"

musl-build:
	rustup target add $(MUSL)
	cargo build --release --target $(MUSL) -p t99-server -p t99-headless -p t99-bot-ext

dist: musl-build
	rm -rf $(DIST)
	mkdir -p $(DIST)/bin $(DIST)/src/crates $(DIST)/examples $(DIST)/utils
	# 二进制（musl 静态）
	for b in $(DIST_BINS); do cp $(MUSL_BIN)/$$b $(DIST)/bin/; done
	# 配置
	cp ruleset.toml server.toml client.toml $(DIST)/
	# 认证：分发环境默认放行所有 PlayerID（server.toml 指向 auth/players.txt）
	mkdir -p $(DIST)/auth
	printf '# 分发环境默认放行所有 PlayerID\n*\n' > $(DIST)/auth/players.txt
	cp dist-README.md $(DIST)/README.md
	cp LICENSE $(DIST)/
	cp examples/pybot.py $(DIST)/examples/
	cp utils/time.py $(DIST)/utils/
	# 源码：dist workspace 根（members 剔除 t99-client / t99-bot-m）
	printf '%s\n' \
	  '[workspace]' 'resolver = "2"' 'members = [' \
	  '    "crates/t99-core",' '    "crates/t99-proto",' '    "crates/t99-config",' \
	  '    "crates/t99-bots",' '    "crates/t99-headless",' '    "crates/t99-server",' \
	  '    "crates/t99-bot-ext",' ']' '' \
	  '[workspace.package]' 'version = "0.1.0"' 'edition = "2021"' 'license = "MIT-0"' '' \
	  '[profile.release]' 'debug = 1' '' \
	  '[profile.release.package."*"]' 'debug = false' > $(DIST)/src/Cargo.toml
	for c in $(SRC_CRATES); do cp -r crates/$$c $(DIST)/src/crates/; done
	cp Cargo.lock $(DIST)/src/
	# t99-bots：剔除 expert 源码并关闭 feature
	rm -f $(DIST)/src/crates/t99-bots/src/expert.rs
	sed -i 's/^default = \["expert"\]$$/default = []/' $(DIST)/src/crates/t99-bots/Cargo.toml
	# 剔除断言：未公开内容与内部产物不得进入分发
	@if find $(DIST) \( -name '*bot-m*' -o -name '*bot-master*' -o -name '*bot-gm*' \
	    -o -name 'expert.rs' -o -name 't99-client' -o -name 'docs' -o -name 'target' \
	    -o -name '*.html' -o -name 'lab' \) | grep -q .; then \
	    echo "[dist] 断言失败：发现应剔除的内容"; exit 1; fi
	@if grep -rE '"crates/t99-(client|bot-m)"' $(DIST)/src/Cargo.toml; then \
	    echo "[dist] 断言失败：workspace members 含应剔除 crate"; exit 1; fi
	@echo "[dist] 剔除断言通过"
	# 自洽性：分发源码必须能独立构建与测试（目标目录临时放置，结束后清理）
	CARGO_TARGET_DIR=$(DIST)/.buildcheck cargo build --release --manifest-path $(DIST)/src/Cargo.toml
	CARGO_TARGET_DIR=$(DIST)/.buildcheck cargo test --manifest-path $(DIST)/src/Cargo.toml
	rm -rf $(DIST)/.buildcheck
	@echo "[dist] 完成：$$(du -sh $(DIST) | cut -f1)，源码自洽构建通过"
	# 部署到题目目录：分发模块 + 同步公开 bot 源码（与 crate 原文逐字一致）
	@if [ -d "$(AX1_DIR)" ]; then \
	    rm -rf "$(AX1_DIR)/dist"; \
	    cp -r $(DIST) "$(AX1_DIR)/dist"; \
	    cp crates/t99-bots/src/basic.rs crates/t99-bots/src/advanced.rs "$(AX1_DIR)/"; \
	    echo "[dist] 已部署到题目目录 $(AX1_DIR)（dist/ + basic.rs + advanced.rs）"; \
	else \
	    echo "[dist] 未找到题目目录 $(AX1_DIR)，跳过部署"; \
	fi

clean:
	rm -rf $(DIST) target/$(MUSL)
