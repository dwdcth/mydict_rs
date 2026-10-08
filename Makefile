GIT_BRANCH ?= $(shell git rev-parse --abbrev-ref HEAD 2>/dev/null || echo dev)
IMAGE ?= mydict-rs
TAG ?= $(shell echo $(GIT_BRANCH) | tr '/' '-')

.PHONY: build setup-multiarch build-multiarch-local test

build:
	docker build --build-arg GIT_BRANCH=$(GIT_BRANCH) -t $(IMAGE):$(TAG) .

setup-multiarch:
	docker run --privileged --rm tonistiigi/binfmt --install all
	docker buildx create --use --name multiarch || true

build-multiarch-local:
	docker buildx build --platform linux/amd64,linux/arm64 \
		--build-arg GIT_BRANCH=$(GIT_BRANCH) \
		-t $(IMAGE):$(TAG) --load .

test:
	cargo test --workspace

# ── TTS（kokoro）构建准备 ───────────────────────────────────────
# pyke 预编译 onnxruntime（glibc>=2.38）本机跑不动：手动下载同款 ms@1.23.2
# 静态库 + isoc23 垫片，ORT_LIB_LOCATION 指过去编译。
# 运行期首次合成还会自动下载 Kokoro 模型 ~337MB 到 ~/.cache/k/（建议挂卷）。
# ORT_TARGET 按构建机架构传（linux x86_64 / aarch64）；macOS/Windows 走
# ort-sys 默认预编译下载（符号没问题），不需要垫片。
ORT_TARGET ?= x86_64-unknown-linux-gnu
ORT_DIR := .cache/onnxruntime-123
ORT_URL := https://cdn.pyke.io/0/pyke:ort-rs/ms@1.23.2/$(ORT_TARGET).tar.lzma2
ifeq ($(ORT_TARGET),x86_64-unknown-linux-gnu)
ORT_SHA := 8c57d059aaaee407812a5698d6706c79e090ad69e1a14204309e802dcbbaa35f
else ifeq ($(ORT_TARGET),aarch64-unknown-linux-gnu)
ORT_SHA := c25248c32d84f228b9d584b84b31e1577e4810d46beb5e304e9fa340c000176c
else
$(error 不支持的 ORT_TARGET: $(ORT_TARGET)（垫片方案只准备 了 linux x86_64/aarch64）)
endif

fetch-ort:
	@mkdir -p $(ORT_DIR)
	@curl -sL -o $(ORT_DIR)/ort.tar.lzma2 $(ORT_URL)
	@echo "$(ORT_SHA)  $(ORT_DIR)/ort.tar.lzma2" | sha256sum -c -
	@python3 -c "import lzma;raw=open('$(ORT_DIR)/ort.tar.lzma2','rb').read();d=lzma.LZMADecompressor(format=lzma.FORMAT_RAW,filters=[{'id':lzma.FILTER_LZMA2,'preset':9}]);open('$(ORT_DIR)/ort.tar','wb').write(d.decompress(raw))"
	@tar -xf $(ORT_DIR)/ort.tar -C $(ORT_DIR)
	@rm $(ORT_DIR)/ort.tar $(ORT_DIR)/ort.tar.lzma2
	@cc -c scripts/isoc23_shim.c -o $(ORT_DIR)/isoc23_shim.o
	@ar rcs $(ORT_DIR)/libisoc23shim.a $(ORT_DIR)/isoc23_shim.o
	@echo "完成：ORT_LIB_LOCATION=$(CURDIR)/$(ORT_DIR) cargo build …"

build-tts: fetch-ort
	ORT_LIB_LOCATION=$(CURDIR)/$(ORT_DIR) cargo build

# ── 本机 systemd 部署（~/.local/bin + ~/.local/share/mydict）─────────────
deploy: build-release deploy-dist
	install -m 755 target/release/mydict target/release/mydict-cli ~/.local/bin/
	systemctl --user restart mydict
	@echo "部署完成：systemctl --user status mydict"

deploy-dist:
	cd frontend && npm run build
	rm -rf ~/.local/share/mydict/dist && cp -a frontend/dist ~/.local/share/mydict/dist

build-release:
	ORT_LIB_LOCATION=$(CURDIR)/$(ORT_DIR) cargo build --release -p server
