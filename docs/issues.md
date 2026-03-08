# tiangong-lca-calculator 问题分析记录

本文档记录当前实现与方法学之间的差距，便于后续排期与验收。

## 0. 当前实现快照

- 技术矩阵 `A`：由 `Input` 边写入，`Output` 只用于建立 provider 映射，不直接写 `A`。
- 环境交换矩阵 `B`：仅对 elementary flow 生效，`Input=-amount`，`Output=+amount`。
- 影响矩阵 `C`：按 flow 对应的 characterization factor 聚合得到。
- 当前仅支持 `strict_unique_provider`：多 provider 的输入边不入 `A`。

代码证据：
- `crates/solver-worker/src/bin/snapshot_builder.rs:642-690`
- `crates/solver-worker/src/bin/snapshot_builder.rs:661-670`
- `crates/solver-core/src/service.rs:267-278`

## 1. 问题一：`B` 中 `Input` 为负，LCIA 会不会出现负值？

### 当前行为

- 会出现负值，且当前实现不会做截断或绝对值转换。
- 计算链路是 `g = Bx`，`h = Cg`，保留线性代数结果原始符号。

### 影响

- 当存在资源摄取、避免负担或补偿逻辑时，`h` 可能为负。
- 如果业务方预期“结果必为正”，会与当前语义冲突。

### 建议处理

- 在契约中明确：LCIA 结果允许负值，负值含义是什么。
- 在 API 返回中增加可选字段：
  - `impact_sign_policy`：`raw` / `non_negative`（默认 `raw`）
  - `raw_h` 与 `reported_h` 区分
- 先不改核心求解，避免破坏数值可比性；展示层按策略处理。

### 已确认方案（建议落地）：双轨结果 `net + gross`

- 不破坏现有数值链路，保留当前净值逻辑：
  - `g_net = B_signed x`
  - `h_net = C g_net`
- 同时新增不抵消视角（毛值）：
  - 构建期将 elementary exchange 拆成两张正值矩阵：
    - `B_out`：仅包含 `Output`，值为正
    - `B_in`：仅包含 `Input`，值为正
  - 求解期新增输出：
    - `g_out = B_out x`
    - `g_in = B_in x`
    - `h_out = C g_out`
    - `h_in = C g_in`
    - `h_net = h_out - h_in`（兼容当前）
    - `h_gross = h_out + h_in`（不抵消）
- API 默认返回 `h_net`，通过可选参数返回 `h_gross/h_out/h_in`。
- 展示层可按场景选择“净影响”或“总压力”，避免单指标误解。

### 落地改造点（草案）

- `snapshot_builder`：拆分并写出 `B_signed/B_out/B_in`。
- `snapshot_artifacts`：扩展 artifact schema（建议新增版本字段/格式版本）。
- `solver-core`：扩展 `SolveResult` 与 `SolveComputationTiming` 输出项。
- `docs/lca-api-contract.md`：补充新字段与兼容策略。
- `bw25-validator`：保持 `net` 对照；新增 `h_net == h_out - h_in`、`h_gross == h_out + h_in` 自一致检查。

### 迁移与兼容原则

- 历史结果与旧客户端默认不受影响（仍读取 `h_net`）。
- 新字段按“可选输出”渐进发布，先灰度后默认开放。

### 验收标准

- 文档明确负值语义。
- `h_net` 与当前版本在同一输入下数值一致（误差阈值内）。
- 新增 `gross` 指标可回放，且满足恒等式：
  - `h_net = h_out - h_in`
  - `h_gross = h_out + h_in`

## 2. 问题二：如何处理 Process 中的 Quantitative reference？

### 当前行为

- builder 目前只读取 exchange 的 `direction + flow_id + amount`。
- 未看到基于 quantitative reference 的归一化逻辑。

### 风险

- 不同 process 若 reference basis 不一致，`A/B` 系数量纲可能不一致。
- 会影响跨过程可比性与总量解释。
- 关键澄清：即使未归一化，`M = I - A` 在数值上仍可能可解，但“可解不等于语义正确”。
  - 数值层面：只要矩阵结构与条件数允许，求解器仍会返回 `x`。
  - 方法学层面：若 `A` 不是统一 reference basis 下的技术系数，`x/g/h` 的单位解释会失真。
  - 结论：不归一化的主要风险是结果语义偏差，而不是程序必然报错。

### 准确性决策（已确认）

- 以“保证计算结果准确性”为目标时，未做 quantitative reference 归一化应判定为**不可接受**。
- 只有在可审计证据证明“上游 exchange 已统一到同一 reference basis”时，才可例外跳过归一化。

### 建议处理

- 在 builder 增加 `reference-normalization` 步骤：
  - 解析 process 的 quantitative reference（reference flow + reference amount + unit）。
  - 将每条 exchange 统一换算到“每 1 reference unit”。
  - 把归一化信息写入 snapshot 诊断，便于追溯。
- 对缺失 reference 的 process 做显式策略：
  - `strict`：报错终止（默认且生产必选）。
  - `lenient`：仅用于调试，不允许进入生产结果链路。

### 验收标准

- 同一 process 任意基准输入输出，归一化后系数一致。
- 快照诊断可追踪每个 process 的 reference 解析结果。
- 在回归测试中补充“未归一化但可求解”的反例，防止把“数值可算”误判为“方法学正确”。
- 生产构建门禁：检测到未归一化或 reference 缺失时，snapshot 构建必须失败。

## 3. 问题三：如何处理 Process 中的 Allocation？

### 当前行为

- 当前构建中未见 allocation 解析与分配逻辑。
- 多功能过程默认按原始 exchange 全量进入，可能与目标方法学不一致。
- 源数据中已出现 exchange 级分配字段：
  - 路径：`processDataSet.exchanges.exchange[].allocations.allocation.@allocatedFraction`
  - 实例值：`"100"`、`"100%"`、以及空 `allocation` 对象。

### 风险

- 多产品过程若不分配，可能重复计入或错误分摊环境负担。
- 与数据库中 ILCD 原始语义偏离，影响结果解释性。

### 建议处理

- 在 builder 增加 allocation 策略层（配置化）：
  - `none`：维持现状。
  - `physical`：按物理量（质量/能量）分配。
  - `economic`：按经济价值分配。
  - `substitution`：系统扩展/替代法（后续阶段）。
- 增加 `allocatedFraction` 解析与归一化规则（P0）：
  - 支持字符串数值与百分号格式（`"25"`、`"25%"`）。
  - 统一转换为比例 `fraction in [0, 1]`。
  - exchange 实际入模值改为：`amount_effective = amount_raw * fraction`。
  - 当 `allocation` 为空或字段缺失：
    - `strict`：报错并中止 snapshot 构建（生产默认）。
    - `lenient`：按 `fraction=1.0` 并记录 warning（仅调试）。
- 每次 snapshot 固化 `allocation_policy` 到 artifact 元数据与 coverage 报告。
- coverage 报告补充分配质量指标：
  - `allocation_fraction_present_pct`
  - `allocation_fraction_missing_count`
  - `allocation_fraction_invalid_count`

### 验收标准

- 同一多功能过程在不同策略下结果可重现、可比对。
- 结果报告包含 allocation 策略与关键参数。
- 对含 `@allocatedFraction` 的样本 process，计算结果与手工按比例缩放一致。
- 对缺失/非法 `@allocatedFraction` 的样本，在 `strict` 模式下必须失败。

## 4. 补充问题：如何处理多个 provider？

### 当前行为

- `strict_unique_provider` 下：
  - 唯一 provider：入 `A`
  - 多 provider：仅计数 `matched_multi_provider`，不入 `A`
  - 无 provider：仅计数 `unmatched_no_provider`，不入 `A`

### 数据现状（数据库/发布数据实测）

- 当前没有“可直接作为 provider 市场份额”的统一字段。
- `process` 输出侧（按 `flow_id`）统计：
  - 总输出 flow：`7158`
  - 多 provider flow：`3760`
  - 单 flow 最大 provider 数：`1979`
- `annualSupplyOrProductionVolume` 覆盖极低且质量不足：
  - 仅 `42/23144` process 有值
  - 其中 `41` 条为同一模板值 `1000kg`
- `percentageSupplyOrProductionCovered` 也偏模板化：
  - 非空 `64` 条，主值为 `50%`（`41` 条）和 `100`（`21` 条）
- exchange 级 `@allocatedFraction` 语义是“共产品分配”，非 provider 市场份额：
  - 有值 `3268/169800` exchanges（约 1.9%）
  - 多数为 `100/100%`，且大量缺失
- `lifecyclemodels` 中存在 `@multiplicationFactor`（24 个模型）可提供部分 share 线索：
  - 覆盖 flow：`1045`
  - 对多 provider flow 的覆盖：
    - 任一 provider 命中：`598/3760`
    - 至少 2 个 provider 命中：`212/3760`
    - 全 provider 覆盖：`38/3760`
  - 结论：可作为 share 信号来源之一，但不能单独覆盖全库。

### 建议处理

- 引入 provider 策略参数：
  - `strict_unique_provider`（当前默认）
  - `best_provider_strict`（证据可区分时仅选一个 provider）
  - `split_by_evidence`（按证据打分分摊）
  - `split_equal`（仅非 strict 模式兜底）
- 不依赖人工补录/审定的 `split_by_evidence` 方案：
  - 对每条“消费 flow + 消费地 + 时间”构建候选 provider 集合。
  - 对候选 provider 计算证据分数 `score_i`，建议由以下信号组成（可配置权重）：
    - `geo_score`：地理匹配（同国家/同州 > 同区域 > 全球）。
    - `time_score`：时间匹配（数据集年份与需求年份越近越高）。
    - `tech_score`：技术相似度（process type/classification/tag 匹配）。
    - `supply_score`：供给规模代理（优先 `lifecyclemodels.@multiplicationFactor`，缺失时退化为中性值）。
    - `quality_score`：数据质量信号（completeness/review/system model 一致性）。
  - 分摊权重：`w_i = score_i / sum(score)`，并强制 `sum(w)=1 ± eps`。
  - 设定可区分阈值：若 `top1/top2 < ratio_threshold` 或 `top1 - top2 < margin_threshold`，视为“不可区分”。
  - `best_provider_strict`：仅在“可区分”时选择 top1；不可区分直接失败（不 silent fallback）。
  - `split_by_evidence`：
    - strict：不可区分即失败。
    - hybrid：不可区分可降级到 `split_equal`，但必须打低置信标记。
- 构建期必须输出诊断与可追溯信息：
  - `provider_strategy`、`provider_mode`、`provider_count`、`resolved_provider_count`
  - `evidence_vector`（每个 provider 各信号分数）
  - `final_weight`、`normalization_residual`
  - `resolution_confidence`（high/medium/low）与 `ambiguity_flag`
- 结果层必须输出不确定性区间（至少三套场景）：
  - `scenario_top1`（仅 top1）
  - `scenario_evidence`（按 `split_by_evidence`）
  - `scenario_equal`（均分基线）
  - 用于度量多 provider 假设对 LCIA 的敏感性。

### Auto-link 规则（仅地理 + 时间）

- 目标：在不依赖人工补录的前提下，为每条输入边自动选择或加权 provider。
- 输入：
  - `flow_id`（被消费的中间流）
  - `consumer_location`（消费过程地点）
  - `consumer_year`（消费过程参考年）
  - `providers`（所有输出该 `flow_id` 的候选过程）
- 候选集：
  - 仅保留与输入流同 `flow_id` 且可作为输出提供者的过程。
  - 若候选为空：记为 `unmatched_no_provider`。
- 地理评分 `geo_score(i)`（建议离散分层，保证可解释）：
  - 同国家同州（或同省/行政区）：`1.00`
  - 同国家：`0.85`
  - 同区域（如 APAC/EU 等）：`0.60`
  - provider 为 `GLO`：`0.40`
  - 无法匹配：`0.10`
- 时间评分 `time_score(i)`（按年份差 `d=|provider_year-consumer_year|`）：
  - `d <= 1`：`1.00`
  - `1 < d <= 3`：`0.85`
  - `3 < d <= 5`：`0.65`
  - `5 < d <= 10`：`0.40`
  - `d > 10`：`0.20`
  - 任一侧年份缺失：`0.50`
- 综合评分（默认地理优先）：
  - `score_i = 0.7 * geo_score(i) + 0.3 * time_score(i)`
  - 仅保留 `score_i >= min_score`（建议 `min_score=0.35`）的 provider。
- 选择与分摊规则：
  - `best_provider_strict`：
    - 若 `top1_score < 0.55`：失败（证据不足）。
    - 若 `top1_score / top2_score < 1.20`：失败（不可区分）。
    - 否则仅选择 `top1`（权重 1.0）。
  - `split_by_evidence`：
    - 对保留集合做归一化：`w_i = score_i / sum(score)`。
    - 要求 `sum(w_i)=1 ± eps`，并记录 `normalization_residual`。
    - strict 下若保留集合为空则失败；hybrid 下可降级 `split_equal`（必须低置信标记）。
- 决定性（determinism）与 tie-break：
  - 当 `score` 完全相同时，按固定键排序：`(geo_score desc, time_score desc, provider_uuid asc)`。
  - 相同输入 + 相同参数必须得到完全一致的 provider 结果。
- 诊断输出（每条输入边）：
  - `auto_link_rule_version`
  - `consumer_location`, `consumer_year`
  - `provider_uuid`, `provider_location`, `provider_year`
  - `geo_score`, `time_score`, `final_score`, `final_weight`
  - `resolution_confidence`, `ambiguity_flag`

### 验收标准

- 覆盖报告输出各策略下的边匹配率和 `A` 变化规模。
- 与 Brightway 对照误差在阈值内。
- `split_by_evidence` 下每条输入边满足：`sum(weights)=1 ± eps`。
- 同一输入在固定参数下权重分配必须确定性可复现。
- strict 模式下不可区分样本必须显式失败，不允许 silent fallback。
- 结果可追溯到 provider 粒度证据向量，并输出置信度与 `scenario_spread`。

## 5. 建议优先级

- P0：Quantitative reference 归一化、Allocation 策略基线。
- P1：多 provider 证据驱动策略（`best_provider_strict` / `split_by_evidence`）。
- P1：接口层结果符号策略（不改核心 `raw_h`）。
