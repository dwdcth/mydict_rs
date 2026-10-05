# MyDict (Rust)

自托管词典服务的 Rust 重写版 —— 原项目 [PoxenStudio/mydict](https://github.com/PoxenStudio/mydict)（Python FastAPI）的全功能对等移植。前端 Vue3 原样保留（零改动），后端以 actix-web + SeaORM 重写。

## 功能

- **三格式词典导入**：MDict v1.2/v2.0/v3.0（mdx+mdd）、StarDict（.ifo/.idx/.dict[.dz]/.syn）、ECDICT CSV
  - MDict 解析采用**双库融合**：[mdictlib](https://crates.io/crates/mdictlib)（v1+v2，惰性迭代、MDD 枚举、LZO）+ [opendict-rs](https://crates.io/crates/opendict-rs)（v3 + StarDict），按头部版本号自动路由，位于 `vendor/opendict` 的 fork 补齐了 MDD 枚举/无缓存打开等接口
  - 导入期解包 .mdd 资源、复制 .mdx 同级附属文件、改写释义内资源引用、展开 `` `N` `` 样式标记
  - 「代」（generation）机制：重新解析原子切换，同名词条全部保留
- **查询**：繁简/全角变体扩展（OpenCC 四配置，与 Python opencc 行为一致——10 万级样本差分 0 差异）、@@@LINK 链式解引用（5 层防环）、精确未命中前缀兜底（注记后缀词头可查）、语言路由 + 误判词典兜底
- **Web 应用**：Vue3 前端 + iframe 沙箱渲染词条（防第三方词典脚本）、多词条聚合文档、暗色主题适配、随机浏览、在线词典（维基百科/维基词典/百度百科）
- **对外 API**：`sk-` Token（SHA-256 哈希 + 每日配额）、开放使用（匿名 IP 限流）、suggest 前缀联想、Token/用户生词本
- **多用户**：管理员/用户双 JWT（HS256，access 2h / refresh 30d）、用户可用词典授权（管理员上限 ∩ 用户自选）、用户 Token（AES-256-GCM 密文存储）
- **运维**：统计四维度（token/user/date/source）+ CSV 导出、查询日志保留期清理（新增）、后台任务进度轮询、维护门（迁移期 503）、CLI（重置密码/修复坏链/语言重识别/迁移）

## 与 Python 版的主要差异（改进）

| 项 | Python 版 | Rust 版 |
|---|---|---|
| 时间戳 | TEXT naive-UTC | INTEGER unix 秒 |
| 可用词典授权 | JSON 列（删词典后残留） | 关联表 + CASCADE |
| query_logs | 无限增长 | 保留期设置 + 定时清理 |
| 用户 Token 明文 | token_plain 列 | AES-256-GCM 密文 |
| 限流计数 | 读-改-写（竞态） | 单语句 UPSERT 原子自增 |
| file_path | "; " 拼接列 | dictionary_sources 子表 |
| LZO | ctypes 垫片直调 liblzo2 | 纯 Rust（lzokay/lzo1x），运行镜像免装库 |
| ORM | SQLAlchemy（SQLite only） | SeaORM（SQLite/PostgreSQL/MySQL） |
| 词条存储 | 全量入库 | 双模式：lite（默认，只落词头+序号，释义运行期按需读源文件，磁盘 ~1x、导入秒级）/ full（释义落库）；`POST /api/admin/dictionaries/{id}/entry-mode` 后台互转，转换走并行解析（`IMPORT_WORKERS`，默认核数一半封顶 4） |
| 上传导入 | 单格式单选 | 多文件/文件夹/zip 拖拽上传，自动解压、识别格式与词典分组（`analyze-upload` + `import-uploaded`） |
| 词典组 | 无 | GoldenDict 式词典组：Web 用户自建命名组（`/api/dict/groups` CRUD），查询时一键切换检索范围 |

## 部署

```bash
cp .env.example .env  # 可选
docker compose up -d --build
# 打开 http://localhost:8000 → 初始化管理员 → 导入词典
```

词典文件放入宿主机 `./data/dicts/`（容器内 `/data/dicts`）后在管理后台「从目录导入」扫描；或直接在网页上传（单次上限默认 512MB）。

数据目录：`./data/{config,db,dictionaries,dicts,logs}`。

### 本地开发

```bash
cargo test --workspace     # 248+ 单元测试 + 11 个 HTTP 契约测试
cargo run -p server        # 后端（默认 0.0.0.0:8000）
./target/debug/mydict-cli  # CLI 工具
cd frontend && npm run build  # 前端构建到 frontend/dist（STATIC_DIR 指向即可托管）
```

## 配置（环境变量）

与 Python 版同名同默认值，另见 `.env.example`。新增：

- `DATABASE_URL`：非空则优先（支持 postgres:// / mysql://），否则用 `DATABASE_PATH` 的 SQLite
- `STATIC_DIR`：前端产物目录（默认 `static`，Docker 内 `/app/static`）

## 环境要求

- Rust ≥ 1.98（mdictlib 依赖；`rust-toolchain.toml` 已锁定）
- 构建 OpenCC FFI 需要 clang + cmake（Docker 镜像已包含）
- 运行镜像需要 speexdec + lame（.spx 发音转码）、tzdata
