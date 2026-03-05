# Edge Function Integration Guide

本文档给 Supabase Edge Functions 项目使用，目标是把前端请求稳定地映射到 `lca_jobs + pgmq` 异步链路。

## 1. 为什么必须走 Edge Function

- 前端不应持有 `service_role`。
- `lca_jobs` 创建、`pgmq.send`、缓存去重都属于受控写操作。
- RLS 已收紧，前端只适合读取自己的 `jobs/results`，不适合写任务。

## 2. 推荐的 Edge API

建议提供以下 API（函数路由名可调整）：

- `POST /lca/solve`
- `GET /lca/jobs/:jobId`
- `GET /lca/results/:resultId`
- `POST /lca/prepare`（管理员/运维）
- `POST /lca/invalidate`（管理员/运维）

## 3. `POST /lca/solve` 输入/输出

### 3.1 请求体（建议）

```json
{
  "scope": "prod",
  "snapshot_id": "optional-uuid",
  "demand": {
    "process_index": 123,
    "amount": 1.0
  },
  "solve": {
    "return_x": true,
    "return_g": true,
    "return_h": true
  }
}
```

Header 建议：

- `X-Idempotency-Key: <uuid-or-hash>`

### 3.2 响应（建议）

首次入队：

```json
{
  "mode": "queued",
  "job_id": "<uuid>",
  "snapshot_id": "<uuid>",
  "cache_key": "<request_key>"
}
```

命中缓存：

```json
{
  "mode": "cache_hit",
  "result_id": "<uuid>",
  "snapshot_id": "<uuid>",
  "cache_key": "<request_key>"
}
```

命中运行中任务：

```json
{
  "mode": "in_progress",
  "job_id": "<uuid>",
  "snapshot_id": "<uuid>",
  "cache_key": "<request_key>"
}
```

## 4. Edge 端处理流程（强约束）

1. 验证用户 JWT，拿到 `user_id`。
2. 解析请求并标准化（默认 `amount=1.0`，补 `solve` 默认值）。
3. 选择 `snapshot_id`：
   - 若请求显式给出，校验存在且可用。
   - 否则读 `lca_active_snapshots(scope='prod')`。
4. 构造 `rhs`（长度 = `process_count`，只在目标 index 赋值 `amount`）。
5. 生成：
   - `request_key`（标准化请求哈希）
   - `idempotency_key`（优先 header，否则退化为 `user_id + request_key`）
6. 在事务中操作：
   - upsert/读取 `lca_result_cache(scope,snapshot_id,request_key)`
   - 若 `ready` 且有 `result_id`，直接返回 `cache_hit`
   - 若 `pending/running` 且有 `job_id`，返回 `in_progress`
   - 否则创建 `lca_jobs(status=queued, requested_by=user_id, request_key, idempotency_key)`
   - `pgmq.send('lca_jobs', payload)`
   - 回写 `lca_result_cache.job_id/status='pending'`
7. 返回 `queued`。

worker 侧会继续推进 `lca_result_cache`：`pending -> running -> ready`（或失败时 `failed`）。

## 5. 与 worker 的职责边界

Edge：

- 鉴权
- 快速参数校验
- 缓存去重与入队
- 结果读取聚合（可选）

worker：

- 取快照数据
- 分解/求解
- 写 `lca_jobs` 终态
- 写 `lca_results`

## 6. 失败与重试建议

- Edge 入队失败：返回 `5xx`，前端可用同 `X-Idempotency-Key` 重试。
- worker 失败：`lca_jobs.status=failed`，`diagnostics.error` 给出原因。
- 前端轮询到 `failed` 时，提示用户重试并保留 `job_id` 便于追踪。

## 7. 最小实现清单

- 使用 service role client（仅服务端）。
- 封装 `resolve_snapshot(scope)`。
- 封装 `build_rhs(process_count, process_index, amount)`。
- 封装 `make_request_key(normalized_input)`。
- 封装 `enqueue_job_and_update_cache(...)` 事务函数。
- 输出统一错误码（如 `BAD_INPUT` / `SNAPSHOT_NOT_READY` / `QUEUE_ERROR`）。

## 8. 不要做的事

- 不要让前端直接写 `lca_jobs`。
- 不要让前端直接调用 `pgmq.send`。
- 不要在 Edge Function 同步等待完整求解结果。
- 不要在 Edge Function 中进行重数值计算。
