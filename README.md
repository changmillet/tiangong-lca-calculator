# Tiangong LCA Calculator

面向 Supabase + Rust + SuiteSparse 的大规模 LCA 稀疏求解服务。

## 1. 架构定位

- Supabase: 业务数据、鉴权、Edge Functions 编排、`pgmq` 队列。
- Rust Solver Worker: 构建稀疏矩阵、缓存分解、重复回代、写回结果。
- SuiteSparse (UMFPACK): 稀疏线性系统求解核心。

核心不变：

- 只解 `M x = y`，其中 `M = I - A`
- 不求显式逆矩阵
- 重计算只走异步 worker，不走前端/同步请求

## 2. 当前实现状态

- 已接入 `snapshot_id` 语义（全库 process 网络）。
- 已支持作业类型：
  - `prepare_factorization`
  - `solve_one`
  - `solve_batch`
  - `invalidate_factorization`
  - `rebuild_factorization`
- 已完成 additive schema：
  - `lca_jobs` / `lca_results`（作业与结果）
  - `lca_network_snapshots`（snapshot 元信息）
  - `lca_snapshot_artifacts`（矩阵 artifact 元信息）
- 已支持结果混合存储：
  - 小结果：写 `lca_results.payload`（JSON）
  - 大结果：写对象存储，`lca_results` 仅存元数据
- 已支持 snapshot artifact-first：
  - builder 直接生成 `M/B/C` 并上传 `HDF5`
  - worker 优先从 `lca_snapshot_artifacts` 下载 artifact，失败才回退到旧 `lca_*_entries` 读取

## 3. 结果文件格式（已选定）

对象存储中的大结果采用：

- 容器：`HDF5`
- 格式标识：`hdf5:v1`
- 文件后缀：`.h5`
- 哈希：`SHA-256`

默认阈值：

- `RESULT_INLINE_MAX_BYTES=262144`（256KB）

当编码后字节数超过阈值时，worker 会上传 artifact 到 S3 兼容存储，并在 `lca_results` 中写入：

- `artifact_url`
- `artifact_sha256`
- `artifact_byte_size`
- `artifact_format`

## 4. 数据库迁移

已提供 migration：

- `supabase/migrations/20260304073000_lca_snapshot_phase1.sql`
- `supabase/migrations/20260304103000_lca_snapshot_artifacts.sql`
- `supabase/migrations/20260304120000_lca_drop_legacy_entry_tables.sql`（清理旧 `lca_*_entries/index` 表）

对已有业务源表（`processes/flows/lciamethods/...`）不做修改。
其中 `20260304120000` 会删除旧的 `lca_*_entries/index` 中间表，只保留 artifact-first 所需表。

可先做静态检查：

```bash
./scripts/validate_additive_migration.sh supabase/migrations/20260304073000_lca_snapshot_phase1.sql
./scripts/validate_additive_migration.sh supabase/migrations/20260304103000_lca_snapshot_artifacts.sql
```

执行迁移：

```bash
set -a && source .env && set +a
psql "$CONN" -v ON_ERROR_STOP=1 -f supabase/migrations/20260304073000_lca_snapshot_phase1.sql
psql "$CONN" -v ON_ERROR_STOP=1 -f supabase/migrations/20260304103000_lca_snapshot_artifacts.sql
psql "$CONN" -v ON_ERROR_STOP=1 -f supabase/migrations/20260304120000_lca_drop_legacy_entry_tables.sql
```

### 4.1 构建可计算 snapshot（artifact-first）

快速生成一个可计算 snapshot：

```bash
./scripts/build_snapshot_from_ilcd.sh
```

默认就是“全库正式版”：

- 只取 `state_code=100`（`--process-states` 默认 `100`）
- 不限 process 数量（`--process-limit` 默认 `0`，即 no limit）
- 生成 coverage 报表到 `reports/snapshot-coverage/<snapshot_id>.{json,md}`
- 报表包含：匹配率、奇异风险、矩阵规模、build 分阶段耗时
- 矩阵 artifact 直接写入 S3（`snapshot-hdf5:v1`）

常用参数：

- `--process-limit 100`：先做小样本调试 snapshot（正式跑不要加）
- `--process-states all`：取消 `state_code` 过滤，按所有 `processes` 构建 snapshot
- `--no-lcia`：先不构建 C 矩阵（只跑到 LCI）
- `--method-id <uuid> --method-version <ver>`：指定 LCIA 方法
- `--self-loop-cutoff 0.999999`：过滤会导致 `M = I - A` 奇异的对角自环（`|A_ii|` 过大）
- `--report-dir <path>`：指定 coverage 报表输出目录

脚本行为：

- 从 `processes/flows/lciamethods` 构建 `A/B/C`（内存）
- 上传 snapshot artifact 到 S3（HDF5）
- 只写 metadata 到：
  - `lca_network_snapshots`
  - `lca_snapshot_artifacts`
- 不修改原始 `processes/flows/lciamethods` 数据
- 不再要求写入大体量 `lca_*_entries` 表
- 默认启用“同源跳过重建”：
  - 基于 `processes/flows/lciamethods` 的 `count(*) + max(modified_at)` 和构建参数计算 fingerprint
  - 若命中已有 `ready` snapshot artifact，则直接复用并秒级返回
  - 若传了 `--snapshot-id`，会按该 ID 执行构建（不走自动复用）
- 冷构建优化：
  - flow 元数据按候选 `id` 查询（避免全表扫 `flows`）
  - process JSON 解析使用并行分片

建议调试流程：

```bash
# 1) 先构建一个小样本可计算 snapshot
./scripts/build_snapshot_from_ilcd.sh --process-limit 100

# 2) 用返回的 snapshot_id 跑 prepare + solve + 结果写回，并记录日志
./scripts/run_full_compute_debug.sh --snapshot-id <snapshot_id>
```

## 5. 环境变量

最小必需：

- `DATABASE_URL` 或 `CONN`
- `PGMQ_QUEUE`（默认 `lca_jobs`）
- `SOLVER_MODE`（`worker` / `http` / `both`）
- `HTTP_ADDR`（默认 `0.0.0.0:8080`）

对象存储（snapshot builder 必需，结果 artifact 建议配置）：

- `S3_ENDPOINT`
- `S3_REGION`
- `S3_BUCKET`
- `S3_ACCESS_KEY_ID`
- `S3_SECRET_ACCESS_KEY`
- `S3_SESSION_TOKEN`（可选，临时凭证时使用）
- `S3_PREFIX`（默认 `lca-results`）
- `RESULT_INLINE_MAX_BYTES`（默认 `262144`）

说明：`S3_ENDPOINT/S3_REGION/S3_BUCKET/S3_ACCESS_KEY_ID/S3_SECRET_ACCESS_KEY` 需要同时提供。上传请求使用 SigV4 签名认证。

## 6. 启动与检查

Ubuntu 依赖：

```bash
sudo apt-get update
sudo apt-get install -y libsuitesparse-dev libopenblas-dev liblapack-dev pkg-config cmake
```

说明：`HDF5` 通过 `hdf5-sys(static)` 在编译期构建，因此需要本机可用 `cmake`。

质量检查：

```bash
make check
```

全量链路调试（prepare + solve + 结果写回 + 日志落盘）：

```bash
./scripts/run_full_compute_debug.sh --snapshot-id <your-snapshot-uuid>
```

低 I/O benchmark 路径（跳过 HDF5 编码与上传，仅 inline 写库）：

```bash
./scripts/run_full_compute_debug.sh --snapshot-id <your-snapshot-uuid> --result-persist-mode inline-only
```

说明：

- 脚本会启动 `solver-worker`（queue 模式）、投递 `prepare_factorization` 和 `solve_one` 两个 job、轮询状态并打印诊断。
- 日志默认写到 `logs/full-run/`，包含：
  - `run-<ts>.log`（执行过程）
  - `worker-<ts>.log`（worker 详细日志）
- 报表默认写到 `reports/full-run/`，包含：
  - `run-<ts>.json`（结构化结果 + 阶段耗时 + `result.compute_timing_sec` + `result.persistence_timing_sec`）
  - `run-<ts>.md`（便于人工查看）
- 若不传 `--snapshot-id`，会自动选最新 snapshot。
- 脚本会优先读取 `lca_snapshot_artifacts` 的矩阵规模；若不存在则回退读取旧 `lca_*_entries`。
- `--result-persist-mode` 支持：
  - `normal`（默认）：大结果走 HDF5 + 对象存储
  - `inline-only`：总是 inline JSON（用于 benchmark，保留可校验结果）

### 6.1 Brightway25 手动校验（默认不触发）

已引入独立校验工具：`tools/bw25-validator`（`brightway25==1.1.1`）。

设计约束：

- 不参与 worker 主链路
- 不自动随 `prepare/solve` 执行
- 仅手动触发，用于数值交叉验证

手动运行：

```bash
./scripts/run_bw25_validation.sh --snapshot-id <snapshot_id>
```

可选指定目标：

```bash
./scripts/run_bw25_validation.sh --result-id <result_uuid>
./scripts/run_bw25_validation.sh --job-id <job_uuid>
```

输出：

- `reports/bw25-validation/<result_id>.json`
- `reports/bw25-validation/<result_id>.md`

校验内容：

- Brightway 重建 `M` 并求 `x`
- 对比 Rust 的 `x/g/h`
- 记录残差与阈值判断（`atol/rtol`）
- 输出速度对比（优先比较“可比计算时间”）：
  - Rust：`solve_mx_sec + bx_sec + cg_sec`（来自 `lca_results.diagnostics.compute_timing_sec`）
  - Brightway：`solve_sec` / `build_plus_solve_sec`
  - 同时保留 `rust_job_run_sec`（含持久化与上传）供端到端参考
- 输出 Rust 持久化拆分耗时（来自 `lca_results.diagnostics.persistence_timing_sec`）：
  - `encode_artifact_sec`
  - `upload_artifact_sec`
  - `db_write_sec`
  - `total_sec`

性能说明（x64 Linux）：

- 校验工具默认安装 `pypardiso`（`pypardiso>=0.4.6`）
- 用于消除 Brightway 在 AMD/Intel x64 上的“未安装 pypardiso”警告并提升线性求解速度

启动服务：

```bash
set -a && source .env && set +a
cargo run -p solver-worker --release
```

## 7. 内部 API

推荐路径（snapshot 语义）：

- `POST /internal/snapshots/{snapshot_id}/prepare`
- `GET /internal/snapshots/{snapshot_id}/factorization`
- `POST /internal/snapshots/{snapshot_id}/solve`
- `POST /internal/snapshots/{snapshot_id}/invalidate`

兼容路径（旧命名别名）：

- `.../models/{snapshot_id}/...`

## 8. Queue Payload 契约

作业 payload 使用 `snapshot_id`。为兼容旧消息，worker 仍接受 `model_version` 字段别名。

## 9. 说明文档

- 面向 AI 的持续上下文：`AGENTS.md`
- 架构与建模方案：`LCA_SCHEMA_PLAN.md`
- 优化评估与优先级：`OPTIMIZATION_REVIEW.md`

## 10. 项目文件整理

当前建议只保留“可复现代码 + 核心文档”，本地运行产物都视为临时文件：

- `logs/`：运行日志（临时）
- `reports/`：调试/验证报告（临时）
- `tools/bw25-validator/.venv/`：本地 Python 环境（临时）

一键清理：

```bash
# 清理 logs/reports/.venv
./scripts/cleanup_local_artifacts.sh

# 仅预览
./scripts/cleanup_local_artifacts.sh --dry-run

# 连同 Rust target 一起清理
./scripts/cleanup_local_artifacts.sh --with-target
```
