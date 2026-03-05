# Frontend Integration Guide

本文档给前端项目使用，目标是稳定触发 LCA 异步计算并正确展示结果。

## 1. 前端只做两件事

- 调 Edge API 发起计算。
- 轮询或订阅 job/result 状态并展示。

前端不要直接写 `lca_jobs` / `pgmq`。

## 2. 推荐交互流程

1. 用户提交功能单位（目标 process + amount + solve 选项）。
2. 前端 `POST /lca/solve`（带 `X-Idempotency-Key`）。
3. 根据返回模式处理：
   - `cache_hit`: 直接拉结果并渲染。
   - `queued` / `in_progress`: 进入 job 进度页。
4. 轮询 `GET /lca/jobs/:jobId`，直到：
   - `completed` / `ready`：查询结果
   - `failed`：展示失败原因
5. `GET /lca/results/:resultId` 后渲染 `x/g/h`（按 UI 需求）。

## 3. 轮询策略

- 初始间隔：`1s`
- 递增到：`2s -> 3s -> 5s`（上限 5s）
- 最长等待：建议 `60-120s`（超时后允许用户继续后台等待）

建议显示：

- 当前状态（queued/running/completed/failed）
- 任务创建时间
- 最近更新时间
- 失败时 diagnostics 摘要

## 4. 结果读取注意点

`lca_results` 有两种形态：

- inline：`payload` 内直接有结果 JSON。
- artifact：结果在对象存储，DB 只保留 `artifact_*` 元数据。

前端建议不直接下载 `artifact_url`，而是经 Edge Function 读取/代理，避免暴露存储细节和权限问题。

## 5. 幂等键建议

每次用户点击“计算”前，生成稳定键：

- 同一请求重复提交（网络重试、刷新）使用同一个 key。
- 请求参数变化（process/amount/solve）必须生成新 key。

可用方式：

- `sha256(user_id + normalized_request_json)`。

## 6. 状态与文案建议

- `queued`: 排队中
- `running`: 计算中
- `ready`: 分解已就绪（prepare 场景）
- `completed`: 计算完成
- `failed`: 计算失败
- `stale`: 快照或分解已过期，需要重建

## 7. 错误处理建议

- `400`：参数问题（提示用户修改输入）
- `401/403`：登录或权限问题
- `404`：任务/结果不存在或不属于当前用户
- `409`：并发冲突（可重试）
- `5xx`：系统异常（带幂等键重试）

## 8. 最小前端接口模型（示例）

```ts
export type SolveSubmitResponse =
  | { mode: 'queued'; job_id: string; snapshot_id: string; cache_key: string }
  | { mode: 'in_progress'; job_id: string; snapshot_id: string; cache_key: string }
  | { mode: 'cache_hit'; result_id: string; snapshot_id: string; cache_key: string };

export type JobStatus =
  | 'queued'
  | 'running'
  | 'ready'
  | 'completed'
  | 'failed'
  | 'stale';
```

## 9. 前端验收清单

- 同一请求重复提交不会生成重复 job。
- 页面刷新后可恢复轮询状态。
- `failed` 能显示可读错误并支持重试。
- 能兼容 inline/artifact 两类结果。
- 用户无法读取他人的 job/result。
