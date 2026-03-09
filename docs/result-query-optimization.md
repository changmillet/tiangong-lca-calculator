# 页面结果查询优化方案（文件化映射版）

更新时间：2026-03-09

## 1. 目标与范围

目标是支持以下两类页面查询：

1. 单个 process，查看其所有 LCIA 值（`process -> all impacts`）。
2. 多个 process，查看某一个 LCIA 值（`many processes -> one impact`）。

约束：

- 不落全量“大结果表”（避免 `snapshot_id + process_index + impact_index -> value` 全量持久化）。
- `snapshot_process_map`、`snapshot_impact_map` 不使用 PG 表存储。
- 保持结果可追溯（明确来自哪个 `snapshot_id`、哪个 `result_id`）。
- 与现有 `solve_one/solve_batch/solve_all_unit` 契约兼容。

## 2. 现状与问题

- `lca_results` 当前只存 artifact 元数据与诊断，不存可直接查询的逐 process 数值。
- `solve_all_unit` 是页面批量查询的理想结果来源，但 artifact 当前是 `hdf5:v1` 包裹 `envelope_json`，查询端无法直接按行随机读取。
- 前端当前仍偏向 `process_index` 交互，不适合业务语义（页面天然是 `process_id`）。

结论：需要“业务 ID 查询 -> 数值索引映射 -> 结果切片读取”的统一链路；其中映射采用文件化（S3）而非 PG 表。

## 3. 方案总览

采用“预计算 + 文件化索引 + 按需切片 + 缓存”的折中方案。

1. 预计算基线：每个 snapshot 维护一份最新 `solve_all_unit(h-only)` 结果。
2. 映射文件化：`process_id/index` 与 `impact_id -> impact_index` 存在 snapshot 索引文件（S3）。
3. 查询走切片：按请求从 result artifact 提取目标值。
4. 查询缓存：Redis 缓存结果，避免重复下载/解析 artifact。
5. 缺失时回退：`all_unit` 不可用时，回退到 `solve_one` 或 `solve_batch`。

## 4. 数据与文件模型

## 4.1 Snapshot 索引文件（必须）

用途：统一承载 snapshot 级映射与元信息。

建议对象路径：

- `snapshots/<snapshot_id>/snapshot-index-v1.json`

建议结构：

```json
{
  "version": 1,
  "snapshot_id": "<uuid>",
  "process_count": 2027,
  "impact_count": 1,
  "process_map": [
    { "process_id": "<uuid>", "process_index": 0, "process_version": "01.00.000" }
  ],
  "impact_map": [
    {
      "impact_id": "<uuid>",
      "impact_index": 0,
      "impact_key": "GWP",
      "impact_name": "Global warming",
      "unit": "kg CO2-eq"
    }
  ]
}
```

说明：

- 推荐作为 snapshot 的 sidecar 文件，不建议塞到 result 文件中。
- 原因：映射是 snapshot 级数据，放 result 会重复且易不一致。

## 4.2 Latest all-unit 指针（轻量）

用途：定位每个 snapshot 当前可用的 `solve_all_unit` 结果。

实现二选一：

1. PG 轻量表（推荐）
   - 仅存 `scope + snapshot_id -> result_id/job_id/status/computed_at`
   - 不存逐 process 数值。
2. S3 manifest 文件
   - 例如 `snapshots/<snapshot_id>/latest-all-unit-v1.json`

建议优先 1（PG 轻量表），便于并发一致性与查询过滤。

## 5. 接口契约（Edge 侧）

新增统一查询 API（建议）：

- `POST /lca/query-results`

请求体（统一）：

```json
{
  "scope": "prod",
  "snapshot_id": "optional-uuid",
  "mode": "process_all_impacts | processes_one_impact",
  "process_id": "optional-uuid",
  "process_ids": ["optional-uuid"],
  "impact_id": "optional-uuid",
  "allow_fallback": true
}
```

说明：

- 对外查询参数使用 `impact_id`（稳定业务键）。
- `impact_index` 仅用于内部切片读取，不作为前端/调用方查询条件。
- 过渡期可兼容 `impact_index` 入参，但应在服务端先映射为 `impact_id` 并输出 deprecate 日志。

模式 A：单 process 全 impacts

```json
{
  "scope": "prod",
  "mode": "process_all_impacts",
  "process_id": "<uuid>"
}
```

模式 B：多 process 单 impact

```json
{
  "scope": "prod",
  "mode": "processes_one_impact",
  "process_ids": ["<uuid-1>", "<uuid-2>"],
  "impact_id": "<impact-uuid>"
}
```

响应体（建议）：

```json
{
  "snapshot_id": "<uuid>",
  "result_id": "<uuid>",
  "source": "all_unit | fallback_solve_one | fallback_solve_batch",
  "mode": "process_all_impacts | processes_one_impact",
  "data": {},
  "meta": {
    "cache_hit": true,
    "computed_at": "2026-03-09T00:00:00Z"
  }
}
```

`data` 结构：

- `process_all_impacts`：`{ "process_id": "...", "values": [ ... ] }`
- `processes_one_impact`：`{ "impact_id": "...", "values": { "<process_id>": 1.23 } }`

## 6. 查询执行流程

1. 鉴权并解析 `scope/snapshot_id`（无显式 snapshot 时走 active snapshot）。
2. 读取 `snapshot-index-v1` 文件，建立 `process_id -> process_index` 与 `impact_id -> impact_index` 映射并校验。
3. 读取 latest all-unit 指针，定位当前 result。
4. 若存在可用 `all_unit` 结果：
   - 读取 result artifact；
   - 提取所需 process/impact 的 `h` 值；
   - 返回并写 Redis 缓存。
5. 若不存在或不可用，且 `allow_fallback=true`：
   - `process_all_impacts` -> `solve_one(return_h=true, amount=1)`；
   - `processes_one_impact` -> `solve_batch(return_h=true, amount=1)`。
6. fallback 结果返回后，仍写 Redis 缓存（短 TTL）。

## 7. 缓存策略

建议两级缓存：

1. 查询结果缓存（Redis）：
   - key：`lca:query:v1:{snapshot_id}:{mode}:{impact_id}:{process_ids_hash}`
   - TTL：5~30 分钟
2. 索引与 artifact 解析缓存：
   - 索引 key：`lca:snapshot-index:v1:{snapshot_id}`
   - artifact key：`lca:artifact:parsed:{result_id}`

说明：

- 当前 `hdf5:v1` 仍需读取 `envelope_json`，缓存是必要的性能手段。

## 8. 三仓改造点

## 8.1 `tiangong-lca-calculator`

- `snapshot_builder` 产出并上传 `snapshot-index-v1` 文件（process_map + impact_map）。
- `solve_all_unit` 完成后维护 latest all-unit 指针（PG 轻量表或 S3 manifest）。
- 提供结果切片读取函数（`result_id + process_indices + impact_index`）。

## 8.2 `tiangong-lca-edge-functions`

- 新增 `lca_query_results` 函数：
  - 输入校验、鉴权、snapshot 解析；
  - 读取 snapshot-index 文件 + latest 指针；
  - 封装切片读取和 fallback；
  - 暴露统一响应结构。

## 8.3 `tiangong-lca-next`

- 页面统一使用 `process_id` 查询。
- 支持两种 UI 模式：
  - 单 process 全 LCIA；
  - 多 process 单 LCIA。
- 显示 `source` 与 `cache_hit`（便于用户理解时延与来源）。

## 9. 分阶段实施

以下拆分用于直接排期与跟踪，状态建议在执行中持续更新为 `todo / doing / done`。

## 9.1 M1（最小可用，先打通链路）

| ID | 仓库 | 任务 | 交付物 | 状态 |
| --- | --- | --- | --- | --- |
| M1-CAL-01 | `tiangong-lca-calculator` | 在 `snapshot_builder` 生成 `snapshot-index-v1.json`（含 `process_map`、`impact_map`、计数与版本） | S3 `snapshots/<snapshot_id>/snapshot-index-v1.json` 可被下载并校验 | todo |
| M1-CAL-02 | `tiangong-lca-calculator` | `solve_all_unit` 完成后写入 latest 指针（PG 轻量表优先；若未落地则用 S3 manifest） | 可通过 `scope + snapshot_id` 定位最新 `result_id` | todo |
| M1-EDGE-01 | `tiangong-lca-edge-functions` | 新增 `POST /lca/query-results`，支持 `process_all_impacts` 与 `processes_one_impact` | API 可读取 snapshot 索引、定位 latest 结果并返回查询值 | todo |
| M1-EDGE-02 | `tiangong-lca-edge-functions` | 加 fallback 逻辑（`solve_one/solve_batch`）与统一 `source` 标识 | 响应中稳定回传 `source/snapshot_id/result_id` | todo |
| M1-NEXT-01 | `tiangong-lca-next` | 页面查询参数切换到 `process_id`，封装两种查询模式 | 单 process 全 LCIA、批量 process 单 LCIA 两个入口可用 | todo |
| M1-NEXT-02 | `tiangong-lca-next` | 前端显示结果来源与缓存命中信息 | UI 可显示 `source`、`cache_hit`、`computed_at` | todo |

M1 完成判定：

- 同一个 `snapshot_id` 下，两种页面查询都能返回结果。
- `all_unit` 缺失时 fallback 能自动触发并成功返回。
- 接口结果可追溯到 `snapshot_id + result_id`。

## 9.2 M2（性能与稳定性）

| ID | 仓库 | 任务 | 交付物 | 状态 |
| --- | --- | --- | --- | --- |
| M2-EDGE-01 | `tiangong-lca-edge-functions` | 增加 Redis 查询缓存（按 `snapshot_id + mode + impact_id + process_ids_hash`） | 热路径明显降时延，重复请求命中缓存 | todo |
| M2-EDGE-02 | `tiangong-lca-edge-functions` | 增加 snapshot-index 缓存与 artifact 解析缓存 | 降低重复下载与重复 JSON 解析开销 | todo |
| M2-EDGE-03 | `tiangong-lca-edge-functions` | 增加请求边界与超时治理（如 `process_ids <= 500`） | 大请求被限制并返回明确错误码 | todo |
| M2-CAL-01 | `tiangong-lca-calculator` | 提供稳定的结果切片读取能力（按 `result_id + indices`） | Edge 不再自行拼装复杂读取逻辑 | todo |
| M2-NEXT-01 | `tiangong-lca-next` | 批量查询分页/分片请求与重试策略 | 页面在大批量场景下可稳定加载 | todo |

M2 完成判定：

- 热缓存查询 p95 `<300ms`。
- 冷缓存且命中 all-unit artifact 查询 p95 `<2s`。
- 批量查询错误率在可接受范围，并有可观测日志。

## 9.3 M3（格式优化，可选）

| ID | 仓库 | 任务 | 交付物 | 状态 |
| --- | --- | --- | --- | --- |
| M3-CAL-01 | `tiangong-lca-calculator` | 评估并实现支持随机切片读取的新 artifact 编码 | 不再依赖全量 `envelope_json` 解析 | todo |
| M3-EDGE-01 | `tiangong-lca-edge-functions` | 适配新 artifact 读取路径并保留旧版兼容 | 灰度期间新旧格式均可查询 | todo |
| M3-OPS-01 | 跨仓 | 迁移脚本与回滚策略 | 可控上线、可回退、可审计 | todo |

M3 完成判定：

- 大多数查询不再触发全量 artifact 解析。
- 线上格式迁移过程中无中断，可回滚。

## 9.4 推荐执行顺序

1. 先做 M1-CAL-01 / M1-CAL-02，保证索引与 latest 指针就绪。
2. 再做 M1-EDGE-01 / M1-EDGE-02，打通查询 API。
3. 然后做 M1-NEXT-01 / M1-NEXT-02，页面接入新 API。
4. M1 验收通过后，再进入 M2 性能治理。
5. M3 仅在查询量和时延压力达到阈值时启动。

## 10. 验收标准

功能：

- 单 process 可稳定返回全 LCIA 值；
- 多 process 可稳定返回某 LCIA 值；
- 无 `all_unit` 结果时 fallback 可用。

一致性：

- 同一 `snapshot_id + process_id + impact_id`，`all_unit` 与 `solve_one/batch` 结果一致（误差阈值内）。

性能：

- 热缓存 p95：`<300ms`；
- 冷缓存（命中 all-unit artifact）p95：`<2s`；
- fallback 触发率可观测且可控。

可运维性：

- 响应中可追溯 `snapshot_id/result_id/source`；
- 错误码区分：映射缺失、结果缺失、超时、权限问题。

## 11. 风险与对策

- 风险：artifact 读取放大导致时延抖动。
  - 对策：两级缓存 + 请求合并 + 限流。
- 风险：snapshot 切换窗口内出现短时不一致。
  - 对策：以 `snapshot_id` 显式对齐查询，响应回传 snapshot。
- 风险：多 process 请求过大。
  - 对策：限制单次 `process_ids` 数量并支持分页/分片。

## 12. 结论

在“不落全量大表 + 映射文件化”的约束下，本方案可以同时满足：

1. 单 process 全 LCIA 页面查询；
2. 多 process 单 LCIA 页面查询；

并保持较好的成本、可追溯性与后续演进空间。
