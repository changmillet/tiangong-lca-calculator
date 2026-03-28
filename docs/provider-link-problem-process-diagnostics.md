# Provider Link Problem Process Diagnostics

本文档记录两类用于定位 provider link / 数据语义异常的诊断方法，目标是从现网 `processes` / `flows` 原始 JSON 中直接筛出“可疑 process 列表”，便于继续追查：

1. `service-loop / provider-loop` 可疑 process
2. `PN / PM0.2 / particle` 语义错配可疑 process

## 背景

在对 `cbaca184-fc86-4fd9-afdf-451ad766792e`（铝锭铸造 / Aluminum ingot casting）的 LCIA 异常排查中，发现了两类高相关问题：

- `Climate change` 等方法出现异常负值，进一步追查发现 hybrid provider linking 下存在服务类 process 自循环 / 互循环，导致求解向量 `x` 出现大量负 activity。
- `EF-particulate Matter` 出现极大负值，进一步追查发现部分 transport process 将 `PN`（particle number）写到了质量型 `Particles (PM0.2)` elementary flow 上，形成明显语义错配。

因此，当前把下面两类查询作为优先级最高的数据网络诊断方法：

- 先抓 “同一 process 内同一 flow 同时作为 input 和 output，且 amount 一样” 的服务环。
- 再抓 “comment 写的是 `PN`，但 flow reference property 是 `Mass`” 的 PM / PN 映射异常。

## 方法 1：Service-loop / Provider-loop 可疑 process

### 诊断逻辑

对每个 process 的最新版本：

- 展开 `processDataSet.exchanges.exchange`
- 找出同一个 `flow_id`
- 在同一 process 内同时存在 `Input` 和 `Output`
- 且 input / output 的 `amount_text` 完全一致

这类记录非常适合抓当前 provider link 语义下的高风险服务环，尤其是：

- 服务流既被 process 作为输入消费
- 又被该 process 自己作为输出提供
- 数值完全相同，表现为“自己生产 1000，再自己消耗 1000”

### 结果解读

输出字段里重点看：

- `process_id`
- `state_code`
- `flow_id`
- `input_exchange_internal_id`
- `output_exchange_internal_id`
- `input_amount_text`
- `output_amount_text`
- `suspicion_tag`

如果命中的是服务流，例如 `Road Transport`，并且 `input_amount_text = output_amount_text = 1000.0`，通常就值得优先排查。

### 已验证命中样本

这条方法已经验证命中以下已知可疑 process：

- `26db822e-6450-4e05-8612-e1de6406e331`
- `b2c0a2f5-1fe5-45a3-a656-97b3f411e8fd`
- `1a7723db-211e-4bbe-9984-3b56969f2a57`
- `d9df832e-e5e4-49b4-948f-85543344b2a3`

这些样本都表现为 `Road Transport` 在同一 process 内 `1000 input / 1000 output`。

## 方法 2：PN / PM0.2 / Particle 语义错配可疑 process

### 诊断逻辑

对每个 process 的最新版本：

- 展开 `processDataSet.exchanges.exchange`
- 只看 `Output`
- 关联 flow 最新版本元数据
- 优先抓以下情况：
  - `comment_text` 包含 `PN`
  - 且对应 flow 的 reference property 是 `Mass`
- 同时保留 `PM0.2` / `particle` 相关 flow 的命中，作为扩展排查入口

其中最强信号是：

- `comment = PN`
- `flow_name = Particles (PM0.2)`
- `reference_property_name = Mass`

这通常意味着“粒子数”被错误地记到了“质量型颗粒物 elementary flow”上。

### 结果解读

输出字段里重点看：

- `process_id`
- `state_code`
- `exchange_internal_id`
- `flow_id`
- `flow_name`
- `reference_property_name`
- `comment_text`
- `amount_text`
- `suspicion_tag`

如果 `suspicion_tag = pn_comment_but_mass_flow`，一般就是高优先级数据问题。

### 已验证命中样本

全库验证时，已命中一批轻型汽车运输相关 process，例如：

- `26db822e-6450-4e05-8612-e1de6406e331`
- `9537caeb-06fd-4538-9dab-8a1a02b28b2e`
- `30e4c738-0bea-4109-8a57-622258024ddf`
- `babdeb0c-9e1b-42f0-9fe5-7230b5d42395`

这些记录都表现为：

- `flow_id = 4d9a8790-3ddd-11dd-9369-0050c2490048`
- `flow_name = 颗粒物 (PM0.2)`
- `reference_property_name = Mass`
- `comment_text = PN`
- `amount_text = 600000000000.0`

## 文件与产物

- 导出脚本：`scripts/export_provider_link_diagnostics.sh`
- SQL 文件：`docs/sql/provider_link_problem_processes.sql`
- 本次导出的 Excel 报表：保存到 `reports/provider-link-diagnostics/`

## 脚本用法

直接导出一份带样式的 Excel：

```bash
./scripts/export_provider_link_diagnostics.sh
```

常用参数：

- `--report-dir <path>`：指定输出目录
- `--output <path.xlsx>`：指定完整输出文件路径
- `--filename-prefix <name>`：自定义文件名前缀

脚本默认会：

- 自动读取仓库根目录 `.env` 中的 `CONN` / `DATABASE_URL`
- 生成 `summary`、`service_loop_candidates`、`pn_pm_candidates` 三个 sheet
- 在 `summary` 中写入中文说明、tag 含义、关键字段含义
- 对结果 sheet 做基础美化：表头样式、冻结首行、自动筛选、自动列宽、按 tag 着色

## 使用建议

- 第一条 SQL 更适合定位会破坏 A 矩阵求解稳定性的 provider / service 环。
- 第二条 SQL 更适合定位会直接污染 biosphere / LCIA 数值的流语义错误。
- 后续如果要继续扩展，可以再补第三类查询：从这些可疑 process 反查“它们影响了哪些 root process / snapshot / result job”。
