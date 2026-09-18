---
title: TIDAS Package Async Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: zh-CN
whenToUse:
  - 当你需要 package-worker 的异步 import/export 契约时
  - 当 package jobs、artifacts、request cache 或 import validation 规则变化时
whenToUpdate:
  - 当 package-worker payload、artifact 格式、状态机或权限边界变化时
checkPaths:
  - docs/tidas-package-contract.md
  - AGENTS.md
  - .docpact/config.yaml
  - crates/solver-worker/**
  - docs/agents/repo-validation.md
  - docs/scope-closure-contract.md
  - docs/agents/contracts/scope-closure-memory-and-result-contract.md
lastReviewedAt: 2026-09-18
lastReviewedCommit: 8520509f27ac372848a92f80ae45d7d7e5e9b828
lastReviewedNote: "Reviewed Worker #295: complete package coverage determines import outcome; bounded terminal importResult projection exposes outcome/counts/report availability, with full details in reports. Validator, transaction, orphan non-import, ownership and gate contracts remain unchanged. Runtime validation evidence is recorded in the task."
related:
  - AGENTS.md
  - .docpact/config.yaml
  - docs/agents/repo-validation.md
  - docs/agents/repo-architecture.md
  - docs/scope-closure-contract.md
  - docs/agents/contracts/scope-closure-memory-and-result-contract.md
---

# TIDAS Package Async Contract

本文档定义 TIDAS 数据包异步导入/导出在 `tiangong-lca-worker` 中的 worker、表结构与 artifact 契约。

## 1. 目标

- 将完整 ZIP 导入/导出从同步 edge function 挪到异步 worker。
- 统一使用 `private.worker_jobs(worker_queue=package)` 生命周期；旧 package job 表与其 PGMQ backend 已退役并 fail closed。
- 避免把 `snapshot_id` 语义强行塞进 package 任务。

## 2. 为什么不复用 `lca_jobs`

数值结果仍强绑定 `snapshot_id` 和数值求解语义，见：

- `private.lca_results.snapshot_id uuid NOT NULL`
- `lca.*` job kind 与 `artifact_format` 也都围绕求解/快照设计

因此 package worker 复用的是“异步模式”，不是“同一张运行时表”。

## 3. 关键表

- `private.lca_package_artifacts`
  - import 源 ZIP、export ZIP、import/export report 的 artifact 元数据
- `private.lca_package_request_cache`
  - 按用户 + 操作 + request key 做去重与状态追踪
- `private.worker_jobs`
  - package worker 的 canonical 生命周期、lease、进度、错误和 result projection

## 4. 队列与 RPC

任务路径：

- `worker_jobs.worker_queue`: `package`
- enqueue RPC: `private.worker_enqueue_job(...)`
- claim RPC: `private.worker_claim_jobs('package', ...)`
- result RPC: `private.worker_record_job_result(...)`
- 仅 `service_role` 可 enqueue / claim / heartbeat / record result

## 5. 任务类型

worker payload `type`：

- `export_package`
- `import_package`

`worker_jobs` 路径使用 job kind 表达统一任务类型，并映射回同一组 package payload：

| `worker_jobs.job_kind` | `payload_schema_version` | payload `type` | result schema |
| --- | --- | --- | --- |
| `tidas.export_package` | `tidas.export_package.request.v1` | `export_package` | `tidas.export_package.result.v1` |
| `tidas.import_package` | `tidas.import_package.request.v1` | `import_package` | `tidas.import_package.result.v1` |
| `tidas.import_package` | `tidas.import_package.request.v2` | `import_package` + `import_policy=root_closure_v2` | `tidas.import_package.result.v1` transport envelope; v2 report artifact |

`package_worker` 走 `worker_jobs` 并领取 `worker_queue=package`。显式选择 `--package-queue-backend pgmq` 会在启动时失败，不会消费消息。`PACKAGE_WORKER_ID`、`PACKAGE_WORKER_JOBS_CLAIM_LIMIT`、`PACKAGE_WORKER_JOBS_LEASE_SECONDS` 控制 worker_jobs claim/diagnostics/lease。

## 6. Payload 契约

### 6.1 `export_package`

```json
{
  "type": "export_package",
  "job_id": "<uuid>",
  "requested_by": "<uuid>",
  "scope": "current_user",
  "roots": []
}
```

`scope` 支持：

- `current_user`
- `open_data`
- `current_user_and_open_data`
- `selected_roots`

### 6.2 `import_package`

```json
{
  "type": "import_package",
  "job_id": "<uuid>",
  "requested_by": "<uuid>",
  "source_artifact_id": "<uuid>"
}
```

`worker_jobs.payload_json` 可以使用上述 snake_case 字段，也可以使用 Edge 友好的 alias：

- `jobId` / `packageJobId` -> `job_id`
- `requestedBy` -> `requested_by`
- `sourceArtifactId` -> `source_artifact_id`
- export roots 中的 `tableName` / `rootTable`、`datasetId`、`datasetVersion` 会映射到 `table`、`id`、`version`

payload 必须仍携带有效 `job_id` compatibility UUID，因为 `lca_package_artifacts`、`lca_package_export_items` 和 `lca_package_request_cache` 的历史 `job_id` columns 仍用于同一次 package 请求内分组与 artifact/cache lookup。该 UUID 不要求存在 `lca_package_jobs` parent row。

## 6.3 v1 `import_package` worker 执行顺序

`import_package` 在 worker 侧执行时，必须先做结构化校验，再进入冲突检测/写库：

1. 下载上传 ZIP artifact；
2. 解压到临时目录；
3. 使用唯一 `TIDAS_BIN`（默认 `tidas`）执行 `version` 与 `validate --describe` 握手，要求精确匹配 `TIDAS_EXPECTED_VERSION`（active governed 默认 `0.2.0`）、公开 validation protocol/profile 和 asset fingerprint；
4. 通过 `tidas validate <dir> --input-format tidas-json --issues <spool> --format json --progress never` 执行结构化校验；issue 必须写入临时文件型有界 spool，operation report 作为有界 JSON 捕获，Worker 对 report schema、完整性、asset fingerprint 以及 spool SHA-256/bytes/event count 全量复核；
5. 若 `summary.error_count > 0`，直接产出 import report：
   - `code = VALIDATION_FAILED`
   - 不执行 conflict checks
   - 不执行任何 inserts
6. 若无校验错误，再执行现有冲突检测和导入流程。

冲突检测以 `table + UUID + 规范化 version` 为精确键。目标环境中已存在的 `state_code = 100..=200` 记录统一视为可复用记录：Worker 跳过 package 中的对应条目，并继续导入其余数据；为保持现有 consumer 兼容，这些记录仍投影到 `filtered_open_data_count` 与 `filtered_open_data`。`state_code` 为 `null` 或不在 `100..=200` 时仍属于 `user_conflicts`，任一此类冲突都会返回 `USER_DATA_CONFLICT` 并阻止整包写入。该导入规则不改变 `open_data` export scope。

### 6.3.1 产品导出暴露隔离（published Result）

本节与上面的 import 规则相互独立。`state_code = 120` 表示已发布的 Result Process；它**不**通过通用产品读取、引用或导出路径暴露，也不得作为产品数据集出现在导出包中。其唯一回读途径是 Database Result 发布命令所拥有的受约束、绑定授权的回执路径；把它扩展为通用公开 Result 读取或导出属于另行授权的产品决定。Worker 通过 Worker-owned 的 `product-export-excludes-published-result-120:v1` policy 实施导出隔离。

实际执行点（逐条按现有读取路径核对）：

| 路径 | 执行方式 |
| --- | --- |
| scope seed（`current_user`） | 根引用查询对 `processes` 增加 `state_code IS DISTINCT FROM 120` 过滤 |
| scope seed（`open_data`） | `state_code = ANY(100..199)` 之上再排除 120（`IS DISTINCT FROM 120`，NULL 安全）；support 表不受影响 |
| seed scan 游标分页 | 每批查询对 `processes` 增加同一 NULL 安全排除条件 |
| exact root / reference scan | `id = ANY(...)` 查询对 `processes` 增加同一排除条件 |
| latest 引用解析 | 复用同一 root-ref 查询 |
| model → process 展开 | 通过 Lifecycle Model 进入的 Process 查询同样排除 120 |
| 最终 hydration | 按 queued item 精确回读，并校验返回集合与 queued 集合完全一致 |
| 最终装配前 | 对 queued `processes` item 做一次 containment 复审 |

语义边界：

- 只有 `processes` 表受该 policy 约束；Flow、FlowProperty、UnitGroup、Source、Contact 等支持数据仍按原有 `100..=199` 语义参与 `open_data` 导出；
- 判定使用 `state_code IS DISTINCT FROM 120`（与 Database 侧同形），不是 `<> 120`：`state_code` 可为 NULL，而 `NULL <> 120` 求值为 UNKNOWN，会静默丢弃此前可导出的 NULL 状态行。NULL 仍然不具数值计算资格，这里只是保持既有的导出行为；
- 该 policy 只排除 `120`；`NULL`、`0`、`20`、`100`、`200` 等既有状态的可导出性不变；公开 `open_data` 的 `100..=199` 谓词本身不被改写；
- `120` 留在既有 import 复用/冲突规则内，import 行为与 `state_code` 语义不变；
- 最终 hydration 若读不到某个已排队 item（被 policy 过滤，或行在 traversal 之后消失），导出**失败**而不是产出一个缺条目的包，因此不会静默发布不完整的依赖闭包；
- `lca_package_artifacts` 的 `export_zip` metadata 记录 `productExportPolicyVersion`，使下载到的包可追溯到产生它的暴露规则；
- 该 policy 不是数值资格规则，也不与 `public-numerical-state-100-excluding-result-120:v1` 互相替代：数值入口按精确 `100` 准入，产品导出按排除 `120` 收敛。

关于状态化路径的准确说法（避免过度声明）：

- policy 生效范围内的**查询过滤**覆盖上述读取路径，包括 resume 时重新执行的 seed scan 与遍历查询；
- 但跨 pass 的 resume 状态（`worker_jobs.diagnostics.seed_scan`、runtime traversal cache、`lca_package_export_items` 中已排队的行）**不会仅因为新旧 policy 不同而被自动失效**。旧策略任务恢复时，已排队的 120 item 会由最终 containment 复审拦下并 fail closed，而不是被静默剔除；
- 已在对象存储中的历史 export artifact 按其精确 id 下载时是历史读取，本 policy 不改写也不重新授权它；
- 因此 `productExportPolicyVersion` 是**可追溯的绑定标记**，不是缓存失效机制。需要主动失效时必须由运维在有界范围内重建对应 job/artifact，而不是依赖该标记。

该 literal 由 Worker 拥有且为本期新增，尚未被 Database 或 Edge artifact 定义；依赖集成前须与 Database #646 的准入/candidate 规则和 Edge 请求契约对齐。

`scope_root_refs_by_user_sql`、`scope_seed_scan_select_prefix_sql` 与 model→process 展开是**活跃路径**：它们分别服务于 `current_user`／`open_data` seed、seed 游标分页和 Lifecycle Model 展开，均已加过滤。`fetch_scope_entries` / `collect_package_entries` 仅保留为未接线的 legacy helper（`#[allow(dead_code)]`），不参与当前导出；保留它们是为了不改变历史行为，而不是作为安全边界。


运行时不得探测 Python module、`tidas-validate` 或其他候选命令。统一 binary 无法启动、版本/协议不匹配、超时、report/spool 不完整或 hash/count 不一致时，任务必须 fail closed，并映射为稳定的 `tidas_*` error code；这些 system failures 不能伪装为数据 validation issue。Worker 继续独立持有 job lease、heartbeat、取消检查、request-cache 状态和 terminal result projection，`tidas` 不接管这些行为。等待长时 validation 时，worker-jobs executor 每个 lease 的三分之一周期续租；heartbeat 被拒绝即丢弃当前 operation future，禁止继续接受或投影 validator evidence。

## 6.4 v2 过程／模型引用链部分导入

`package_import_v2.rs` 只处理显式 v2 policy。包内扫描、身份／重复检测、全部 issue spool 消费、引用图及候选引用链复验全部完成后，才进入业务表写入；不以数据库补齐缺失引用，也不以 `state_code` 跳过包内校验。每条过程和模型均为独立根对象，按完整直接／间接引用闭包传播阻断；孤立支持数据不写入，环通过已访问集合终止。

v1 与 v2 共用 `run_tidas_package_command`，保持同一精确 binary 握手、assets、参数和 `summary.validation.error_count` 门槛。独立的 eILCD/XSD/roundtrip 输出保留为证据，本改造不把它们提升为新门槛。全包 native error 可能提前结束后续阶段，因此候选闭包继续使用同一完整命令复验。显示样本的 1,000 条限制不参与判定；原始 JSON 字节与来源路径保留。

完整计划绑定 source SHA、policy、validator/assets 和计划摘要。每个成功候选调用 Database `private.tidas_import_group_apply_v2`：同事务写入根对象／依赖和成功回执，`ON CONFLICT(id,version) DO NOTHING` 处理所有已有状态。分组只插入，不覆盖、不比较数据库内容。模型与过程在同一分组时复用原有 `backfill_process_model_ids`；失败模型不自动阻断独立过程。共享记录全局去重计数。只有 serialization/deadlock 最多重试两次；约束错误回滚当前组，连接／lease 错误停止后续组。最终 lease fence 防止失租提交。

v2 `import_report` 使用 `tidas-package-import-report:v2`，业务 `outcome` 为 `success/partial/none/interrupted`；Worker completed 仅表示执行返回。`roots` 最多 100 条，完整 `import_details` ZIP 包含全部 issues、validation、references、roots、records 和 plan NDJSON，manifest 绑定各文件大小与 SHA。records 的 ordinal 对应 plan 节点编号。系统失败放入 execution error，不伪装为数据校验 issue。准备失败不会入库；报告上传中断后，数据库回执仍是已提交事实，`api.svc_tidas_package_read_v2` 提供 owner-scoped 计数。两个报告制品复用现有 14 天导入保留期。

终态结果按完整包覆盖判定：执行未中断、至少一个根分组成功、全部根成功且 `not_imported_count=0` 才是 `success`。已有记录复用计入覆盖；存在成功根但有任意未成功分组或未导入记录（包括未被任何根引用的孤立 Contact 等）为 `partial`。没有根或没有成功根为 `none`；执行中断为 `interrupted`，即使已有组提交也不会变成成功。`records.ndjson` 为未导入条目增加 `not_imported_reason=unreferenced|group_not_imported`，分别表示不在任何根的闭包内、或所在组未成功；原有不写入孤立数据的行为不变。

终态 `worker_jobs.result_json.importResult` 是列表可直接使用的有限摘要：`outcome`、`executionComplete`、`summary` 中固定十二个非负计数字段，以及 `reportAvailable` / `detailsAvailable`。报告可用标记是终态发布时的快照，只有 ready artifact 为 true；下载时仍必须通过 owner-scoped API 重新校验当前制品状态并生成签名链接。准备失败后若已发布报告，失败结果也尽力投影该摘要；投影读取失败不能掩盖原始任务失败。旧 v1 和未包含摘要的历史任务仍可按原 job ID 请求报告。此字段为 result.v1 transport 的增量字段，不添加新的 endpoint 或数据库迁移；完整记录、路径、issues 仍仅存于报告。

显式容量为 ZIP 512 MiB／解压 2 GiB／文档 16 MiB／数据 100,000 条／引用 1,000,000 条／根 2,000 条；每组 50,000 条且 64 MiB，引用链复验累计文档访问上限 2,000,000。问题证据流及校验摘要流分别最多 512 MiB；单条问题最多 16 MiB，界面问题样本最多 1,000 条且 8 MiB。超限必须明确失败，不可截断成成功。

部署顺序：先发布兼容 v1/v2 的 Worker 与 Database 增量迁移，再发布 Edge，最后启用 Next v2 提交。必须保留历史 v1 jobs/reports 的读取路径。

## 7. Artifact 契约

`lca_package_artifacts.artifact_kind`：

- `import_source`
- `export_zip`
- `export_report`
- `import_report`

`artifact_format`：

- `tidas-package-zip:v1`
- `tidas-package-export-report:v1`
- `tidas-package-import-report:v1`

推荐 `content_type`：

- ZIP: `application/zip`
- report: `application/json`

### 7.0.1 大 artifact 上传上限

package worker 通过 S3-compatible object storage 写入 export ZIP / report artifact。对象存储平台的真实 max-file-limit 必须大于预期 package artifact 体积；例如全量 `open_data` export 可能达到数百 MB。若生产后端限制低于 artifact 体积，运维应优先调高平台侧 max-file-limit。

`S3_MAX_UPLOAD_BYTES` 是 worker 本地 preflight guard，应配置为与平台侧 max-file-limit 一致或略低。设置后，worker 会在 single PUT 或 multipart upload 发起前检查 artifact byte size；超限时使用 `artifact_too_large` 失败诊断，并保留 `upload_mode`、`stage=preflight_upload_size`、`artifact_byte_size`、`max_upload_bytes` 和 `storage_error_code=EntityTooLarge`，避免 multipart 上传到中途才失败。

### 7.1 Artifact retention / GC 契约

package artifact 必须带或刷新 `expires_at`：

- `export_zip` / `export_report`：默认 30 天；
- `import_source` / `import_report`：默认 14 天；
- worker 写入的新 artifact 在插入时写入 `expires_at`；
- `import_source` 由 API 上传创建时，worker 在 import job 进入 terminal 成功/失败状态后刷新 14 天 TTL；
- `is_pinned = true` 的 artifact 不参与自动 GC；
- `status = deleted` 表示对象 payload 已被 GC 删除，API 不应再返回可下载 URL。

worker 侧 GC 必须 object-aware：

1. dry-run 先输出 eligible/protected reason；
2. 每次运行固定一个 `as_of`，候选扫描、复核、cache/detail cleanup 都使用同一截止时间；
3. 只处理 `expires_at <= as_of`、`is_pinned = false`、无 `queued/running/waiting` canonical 或 legacy parent、且无 active/recent request-cache 引用的 ready artifact；缺失 parent 本身不是永久保护条件；
4. 每个 candidate 在对象删除前按同一 `as_of` 再复核一次；
5. 先删除对象存储 payload；对象已不存在按幂等成功处理；
6. 对象删除成功后，才把 artifact 标记为 `deleted`；
7. 对象删除失败时只记录 `metadata.gc` 错误，不删除 DB metadata；
8. artifact GC 后再以 bounded `FOR UPDATE SKIP LOCKED` batch 清理 stale request-cache 与无 live artifact/cache/active-parent 保护的 export-item detail；
9. canonical `private.worker_jobs` history 永不由 package GC 删除。数据库 `util.apply_lca_package_retention(...)` 仅保留 dry-run preview，`p_dry_run=false` 必须 fail closed。

当前 worker runtime 提供 `package_gc` CLI：

```bash
cargo run -p solver-worker --bin package_gc --
cargo run -p solver-worker --bin package_gc -- --execute
```

切到统一 `worker_jobs` 后，package artifact GC 使用 `job_kind=tidas.package_artifact_gc`、`worker_queue=maintenance`。timer/operator action 通过 `maintenance_enqueue package-artifact-gc` 创建任务；`maintenance_worker` 领取任务后仍调用现有 `package_gc` binary。payload 表达 `execute`、`batchSize`、`maxBatches`、`jobRetentionDays` 和 `requestCacheRetentionDays`，result 记录 parsed `[summary]`、exit code、stdout/stderr tail，并通过 `result_ref` 指向 operator-only `maintenance_gc_report` artifact metadata row。缺省不传 `execute=true` 时必须保持 dry-run 行为。

生产部署契约：

- `package_gc` release binary 应随 `package_worker` 一起部署到所有活跃 worker 主机；
- checked-in `deploy/systemd/package-gc.service` 与 `package-gc.timer` 只能在一个调度主机启用，其他主机保留 binary 作为故障切换候选；
- 统一队列模式下，timer 或 operator action 只负责 enqueue `tidas.package_artifact_gc` worker job，不直接代表任务事实；
- timer 首次启用必须 dry-run，不带 `--execute`，并检查 `[retention]` eligible/protected reason 与 `[summary] dry_run=true ...`；
- timer 默认每天 `03:15 UTC` enqueue 一次，并增加最多 15 分钟随机延迟；dry-run 与 execute 的默认 idempotency key 都按 UTC 日期分桶，同日重复触发复用同一 maintenance job，包括已到终态的 job；
- destructive 清理必须显式加 `--execute`，并在首轮保留小批量限制，例如 `PACKAGE_GC_BATCH_SIZE=100`、`PACKAGE_GC_MAX_BATCHES=1`；
- `--execute` 模式需要对象存储环境变量，且会先删对象 payload，再标记 artifact `deleted`；对象删除失败时只记录 `metadata.gc` 错误，不删除 DB metadata；
- `--execute` 模式使用 PostgreSQL advisory lock 防止重叠执行；在统一队列模式下还必须使用 `worker_jobs.concurrency_key` 防止同环境同类 GC 并发；
- destructive execute job 默认 `max_attempts=1`，失败后由 operator 显式 retry，避免删除类任务自动重复执行。

### 7.2 import report payload（新增字段）

`tidas-package-import-report:v1` 的 payload 结构扩展如下：

- `summary.validation_issue_count`
- `summary.error_count`
- `summary.warning_count`
- `validation_issue_sample_limit`（固定 `1000`）
- `validation_issues_truncated`
- `validation_issues[]`

`validation_issues[]` 每条包含：

- `issue_code`
- `severity`
- `category`
- `file_path`
- `location`
- `message`
- `context`

无论最终结果是 `IMPORTED` / `USER_DATA_CONFLICT` / `VALIDATION_FAILED`，report 都会携带完整校验统计。`validation_issues[]` 是按确定性 spool 顺序保留的前 `validation_issue_sample_limit` 条样本；总数大于样本上限时 `validation_issues_truncated=true`，完整事实仍由 `summary.validation_issue_count`、severity counts 以及已验证的原始 spool hash/bytes/event count 约束。Worker 必须流式验证 spool，不得把大包的完整 JSONL 或全部反序列化 issue 同时保留在内存。

### 7.3 与 certificate-bound LCIA result package 的边界

`lcia_result.package_build` 属于 solver queue 的 data-product 构建，不是本文件定义的 `tidas.export_package` / `tidas.import_package` package-worker 任务。Build V2 必须携带完整 scope-closure certificate/snapshot/bundle/report binding；solver worker 在构建前 fail-closed 校验该 binding，然后复用既有数值 snapshot、all-unit solve 和 artifact 路径，不重新运行 administrative closure。数值 HDF5 不再要求内嵌完整 `CompiledGraph`；Calculation Bundle 所需 release metadata 由 HDF5 descriptor 指向 `snapshot-release-evidence-json-zstd:v2`，后者再绑定 `snapshot-source-closure-json-zstd:v1`，materialization 逐层校验 size/SHA-256/format/content type/dataset count。Legacy numerical payload 即使 graph schema 漂移也仍可读；schema-compatible graph-bearing snapshot 与 v1 full-evidence sidecar 可用于 bundle，incompatible evidence 明确要求重建。

可用于该 binding 的新 certificate-grade Scope Closure 请求固定冻结 `technosphereBoundaryPolicy=cutoff`；provider gap 仍保留为 warning/evidence，但不单独阻断证书。这个约束不改变 TIDAS import/export package 的 payload、状态机或校验规则。

Fresh scope closure uses the bounded `lcia.scope-closure-bundle.v4` binding manifest. Package verification downloads that JSON through the bounded file API, recomputes its exact hash, and streams only the schema/token binding fields; historical v1/v3 bundle files remain readable without materializing the complete object in memory. The growing administrative evidence is partitioned under the closure manifest and does not change TIDAS import/export ZIP semantics.

这项绑定不会改变 TIDAS import/export ZIP、report、retention 或 `worker_queue=package` 状态机。完整契约见 `docs/scope-closure-contract.md` 与 `docs/lca-api-contract.md`。

Solver queue 的 `lcia_result.package_build.request.v3` 可在同一证书和 Calculation Bundle 上额外生成 Portal typed projection；它不进入 `worker_queue=package`，不读取或改写 import/export ZIP，也不改变本文件的 report/retention contract。该投影契约见 `docs/agents/contracts/portal-lcia-projection-contract.md`。

`worker.artifact_gc` 当前消费 Database Engine 管理的通用临时 artifact lifecycle contract，首先覆盖七天 scope-closure evidence。它不替代 `tidas.package_artifact_gc`、不改变本文件的 14/30 天 package retention、pin/cache/job protection 或 package-specific metadata cleanup。未来若 Database contract 将 package artifacts 纳入同一通用 claim surface，必须先在 database-engine 与本文件中显式协调迁移。

## 8. 状态机

`worker_jobs` 路径外层生命周期：

- `queued/stale -> running -> completed|failed|cancelled`
- `phase` 使用 `export_package` 或 `import_package`
- `progress` 仅用于任务中心提示，不替代 package artifact 或 request-cache 状态
- terminal `result_json` 包含 `workerJobId`、`packageJobId`、`payloadType`、`packageJobStatus`、`artifacts[]`
- `result_ref` 使用 `{"domainSource":"worker_jobs","workerJobId":"<uuid>","packageJobId":"<uuid>"}`，并且 worker 会把 `lca_package_artifacts`、`lca_package_export_items`、`lca_package_request_cache` 中可关联的 rows 回填到同一个 `worker_job_id`

重要差异：

- worker runtime 在同一个 worker job lease 内连续执行 export pass，并在 pass 间 heartbeat；
- 长导出的 seed-scan continuation state 以 `worker_jobs.diagnostics.seed_scan` 为 canonical resume source；worker 从最新匹配 `tidas.export_package` worker job diagnostics 恢复游标；
- 因此 `PACKAGE_WORKER_JOBS_LEASE_SECONDS` 必须大于正常单 pass 时间，长导出仍应依赖 pass 间 heartbeat 续租。

## 9. 权限边界

- 前端不直接写 `worker_jobs`
- Edge Functions 负责鉴权、幂等和入队
- worker 负责大包处理、对象存储写入、导入冲突规则执行
- `authenticated` 仅可读自己的 package jobs / artifacts / request cache
- `service_role` 保留完整权限
