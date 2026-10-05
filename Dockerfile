# 三阶段构建：前端 → 后端 → 运行镜像（对齐 Python 版单容器设计）
#
# 构建参数：
#   GIT_BRANCH  → 写入 /version.txt（版本展示，与 Python 版一致）
#   MIRROR      → Debian apt 镜像（默认国内源，Docker build 传入空串用官方源）

FROM node:20-alpine AS frontend-build
WORKDIR /app
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1.98-slim AS backend-build
ARG GIT_BRANCH=dev
WORKDIR /build
# 系统依赖：libclang + cmake（opencc-sys 的 vendored OpenCC 需要）
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev cmake make g++ \
    && rm -rf /var/lib/apt/lists/*
# 先拷依赖清单，利用 docker layer cache
COPY Cargo.toml Cargo.lock ./
COPY crates/dict-parser/Cargo.toml crates/dict-parser/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/migration/Cargo.toml crates/migration/Cargo.toml
COPY vendor/opendict/Cargo.toml vendor/opendict/Cargo.toml
RUN mkdir -p crates/dict-parser/src crates/server/src crates/migration/src vendor/opendict/src \
    && echo "pub fn placeholder() {}" > crates/dict-parser/src/lib.rs \
    && echo "pub fn placeholder() {}" > crates/server/src/lib.rs \
    && echo "pub fn placeholder() {}" > crates/migration/src/lib.rs \
    && echo "pub fn placeholder() {}" > vendor/opendict/src/lib.rs \
    && cargo build --release 2>/dev/null || true
# 真实源码
COPY vendor/opendict vendor/opendict
COPY crates crates
RUN touch crates/dict-parser/src/lib.rs crates/server/src/lib.rs crates/migration/src/lib.rs vendor/opendict/src/lib.rs \
    && cargo build --release -p server
RUN echo "${GIT_BRANCH}" > /version.txt

FROM debian:12-slim
# speexdec + lame：.spx→.mp3 按需转码；tzdata：TIMEZONE 设置；ca-certificates：在线词典出站
RUN apt-get update && apt-get install -y --no-install-recommends \
    speex lame tzdata ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /usr/bin/lame-*.1 2>/dev/null || true
WORKDIR /app
COPY --from=backend-build /build/target/release/mydict /app/mydict
COPY --from=backend-build /build/target/release/mydict-cli /app/mydict-cli
COPY --from=backend-build /version.txt /version.txt
COPY --from=frontend-build /app/dist /app/static
ENV STATIC_DIR=/app/static \
    VERSION_FILE_PATH=/version.txt
VOLUME /data
EXPOSE 8000
# 单进程：进程内缓存/限流/任务表的前提（对齐 Python uvicorn --workers 1）
ENTRYPOINT ["/app/mydict"]
