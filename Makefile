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
