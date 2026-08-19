SHELL := /bin/bash

ROOT := $(abspath .)
HARNESS := $(ROOT)/harness
VERSION := $(shell sed -n 's/^version = "\([^"]*\)"/\1/p' "$(HARNESS)/Cargo.toml" | head -n 1)
APP := $(HARNESS)/dist/AIOPS Desktop.app
DMG := $(HARNESS)/dist/AIOPS-Desktop-$(VERSION).dmg
WORKSPACE ?= $(ROOT)
NOTARY_PROFILE ?= aidops-notary

.DEFAULT_GOAL := help

.PHONY: help doctor install-dev-tools watch-ready dev dev-replay dev-watch \
	dev-replay-watch check test fmt icon build release \
	package package-mac-dev package-mac-release package-mac-notarize \
	package-mac-universal verify-mac open-mac logs

help: ## 显示快捷命令
	@printf '%s\n' \
	  'AIOPS Desktop 开发与发布命令' \
	  '' \
	  '开发调试' \
	  '  make doctor              检查 Rust、macOS 工具和签名证书' \
	  '  make install-dev-tools   安装 cargo-watch 开发工具' \
	  '  make dev                 启动 GUI 开发版' \
	  '  make dev-replay          使用离线 Replay 模型启动 GUI' \
	  '  make dev-watch           监听源码，自动重编译并重启 GUI' \
	  '  make dev-replay-watch    离线 Replay 热重启开发模式' \
	  '  make check               全 workspace 编译检查' \
	  '  make test                运行全 workspace 测试' \
	  '  make fmt                 仅检查 Rust 格式，不改文件' \
	  '  make logs                跟踪 GUI 诊断日志' \
	  '' \
	  '构建与资源' \
	  '  make icon                重新生成 Windows/macOS/运行时图标' \
	  '  make build               Debug 构建' \
	  '  make release             Release 全功能构建' \
	  '  make package             当前平台默认打包' \
	  '' \
	  'macOS 打包' \
	  '  make package-mac-dev     Apple Development 本地测试包' \
	  '  make package-mac-release Developer ID 正式签名，不上传' \
	  '  make package-mac-notarize 正式签名、Apple 公证并 staple' \
	  '  make package-mac-universal Universal 2 正式公证包' \
	  '  make verify-mac          验证 app、DMG 签名和公证票据' \
	  '  make open-mac            启动 dist 中的 macOS app' \
	  '' \
	  '可覆盖变量' \
	  '  WORKSPACE=/path          GUI 默认工作区' \
	  '  NOTARY_PROFILE=name      notarytool 钥匙串配置名'

doctor: ## 检查本机开发和 macOS 发布环境
	@command -v cargo >/dev/null && cargo --version
	@command -v rustc >/dev/null && rustc --version
	@command -v python3 >/dev/null && python3 --version
	@if [[ "$$(uname -s)" == "Darwin" ]]; then \
		command -v xcrun >/dev/null && xcrun --version; \
		printf '\n可用代码签名证书:\n'; \
		security find-identity -v -p codesigning; \
	else \
		printf 'macOS 签名检查已跳过（当前系统: %s）\n' "$$(uname -s)"; \
	fi
	@if command -v cargo-watch >/dev/null; then \
		cargo watch --version; \
	else \
		printf '\ncargo-watch: 未安装（运行 make install-dev-tools）\n'; \
	fi

install-dev-tools: ## 安装自动重启开发工具
	cargo install cargo-watch --locked

watch-ready:
	@command -v cargo-watch >/dev/null || { \
		echo '错误：未安装 cargo-watch。请先运行 make install-dev-tools'; \
		exit 1; \
	}

dev: ## 启动 GUI 开发版
	cd "$(HARNESS)" && HARNESS_WORKSPACE="$(WORKSPACE)" cargo run -p harness-bin -- --gui

dev-replay: ## 使用离线 Replay 模型启动 GUI
	cd "$(HARNESS)" && HARNESS_WORKSPACE="$(WORKSPACE)" HARNESS_REPLAY=1 cargo run -p harness-bin -- --gui

dev-watch: watch-ready ## 监听源码并自动重启真实模型 GUI
	cd "$(HARNESS)" && HARNESS_WORKSPACE="$(WORKSPACE)" cargo watch --delay 0.3 -x 'run -p harness-bin -- --gui'

dev-replay-watch: watch-ready ## 监听源码并自动重启 Replay GUI
	cd "$(HARNESS)" && HARNESS_WORKSPACE="$(WORKSPACE)" HARNESS_REPLAY=1 cargo watch --delay 0.3 -x 'run -p harness-bin -- --gui'

check: ## 全功能编译检查
	cd "$(HARNESS)" && cargo check --workspace --all-features

test: ## 全功能测试
	cd "$(HARNESS)" && cargo test --workspace --all-features

fmt: ## 检查 Rust 格式但不修改文件
	cd "$(HARNESS)" && cargo fmt --all -- --check

icon: ## 生成全平台图标资源
	cd "$(HARNESS)" && python3 scripts/make_icon.py

build: ## Debug 构建
	cd "$(HARNESS)" && ./scripts/build.sh build

release: ## Release 全功能构建
	cd "$(HARNESS)" && cargo build --release --all-features

package: ## 当前平台默认打包
	cd "$(HARNESS)" && ./scripts/build.sh package

package-mac-dev: ## Apple Development 本地测试包
	cd "$(HARNESS)" && MACOS_SIGNING_MODE=development ./scripts/build.sh package

package-mac-release: ## Developer ID 正式签名包，不上传公证
	cd "$(HARNESS)" && MACOS_SIGNING_MODE=release ./scripts/build.sh package

package-mac-notarize: ## Developer ID 签名并提交 Apple 公证
	cd "$(HARNESS)" && MACOS_SIGNING_MODE=release MACOS_NOTARY_PROFILE="$(NOTARY_PROFILE)" ./scripts/build.sh package

package-mac-universal: ## Universal 2 正式签名并公证
	cd "$(HARNESS)" && MACOS_UNIVERSAL=1 MACOS_SIGNING_MODE=release MACOS_NOTARY_PROFILE="$(NOTARY_PROFILE)" ./scripts/build.sh package

verify-mac: ## 验证 macOS 签名和公证票据
	codesign --verify --deep --strict --verbose=2 "$(APP)"
	codesign --verify --verbose=2 "$(DMG)"
	xcrun stapler validate "$(DMG)"
	spctl --assess --type execute --verbose=2 "$(APP)"
	shasum -a 256 "$(DMG)"

open-mac: ## 启动已打包 macOS app
	open -n "$(APP)"

logs: ## 跟踪 GUI 诊断日志
	@log="$$HOME/Library/Application Support/com.clotee.aidops/harness_gui_trace.log"; \
	touch "$$log"; \
	echo "$$log"; \
	tail -f "$$log"
