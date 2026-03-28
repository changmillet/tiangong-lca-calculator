# Provider Linking Current State

更新时间：2026-03-28

本文档用于梳理 `tiangong-lca-calculator` 当前的 provider link 逻辑，并基于线上最新 snapshot / 成功计算样本，说明 provider link 的当前状态、历史基线问题与后续修改策略。

补充说明：

- 2026-03-28 当天，基于同一批线上 process 数据完成了四种 provider rule 的 replay 对比。
- 根据 replay 结果，`snapshot_builder` / worker 的默认 `provider_rule` 已切换为 `split_by_evidence_hybrid`，线上 active snapshot 也已切到该规则。
- 单 provider case 仍然保留“唯一 provider 直接写入 `A`”的路径；变化只发生在 multi-provider 分支。

当前文档有两层用途：

- 第一层：说明“现在的线上状态是什么”，尤其是 multi-provider link 是否已经打通。
- 第二层：保留 2026-03-28 当天对 `strict_unique_provider` 旧基线的 replay 分析，作为为什么要切默认规则的历史依据。

目标不是直接给出最终方案，而是先把：

- 当前逻辑到底怎么工作
- 线上卡点到底在哪里
- 哪些问题是配置导致
- 哪些问题是数据形态导致
- 哪些问题是规则能力不足导致

讲清楚，方便后续继续分析和改逻辑。

---

## 1. 相关代码与数据入口

核心代码：

- `crates/solver-worker/src/bin/snapshot_builder.rs`
- `crates/solver-worker/src/compiled_graph.rs`

现有说明：

- `docs/issues.md`
- `AGENTS.md`

线上诊断与指标来源：

- `public.lca_network_snapshots`
- `public.lca_snapshot_artifacts`
- `public.lca_jobs`
- `public.lca_latest_all_unit_results`

辅助 SQL：

- `docs/sql/2026-03-28-provider-a-metrics.sql`

---

## 2. 当前 provider link 逻辑

### 2.1 逻辑所在阶段

provider link 发生在 snapshot build 阶段，而不是 solve 阶段。

当前链路是：

1. 从 `processes` 读取 process JSON
2. 解析每个 process 的 exchanges
3. 基于 `Output` exchanges 生成 provider 候选集合
4. 基于 `Input` exchanges 查找 provider
5. 生成 technosphere edges
6. 把 technosphere edges 聚合写入 `A`
7. 由 `A` 构造 `M = I - A`

也就是说，provider link 的结果直接决定：

- 哪些 input edge 能进 `A`
- `A` 的稀疏结构
- `a_input_edges_written`
- 结果层是否真正反映供应链网络，而不只是局部 inventory

### 2.2 候选 provider 是如何生成的

当前 provider 候选集合是按下面这条规则生成的：

- 遍历所有 process 的 exchanges
- 只要某个 exchange 是 `Output`
- 就把这个 process 记为该 `flow_id` 的 provider 候选

代码上等价于：

- `flow_id -> { provider_process_idx... }`

关键点：

- 候选集合是按 `flow_id` 聚合的
- 当前没有在候选生成阶段继续按单位、reference product 语义、功能类型、市场/技术类型等更细信息收窄

这意味着：

- 只要多个 process 都对同一个 `flow_id` 有 `Output`
- 它们就会全部进入同一个 provider 池

这也是当前多 provider 候选数偏大的根本原因之一。

### 2.3 Input edge 是如何处理的

对于每条 `Input` exchange：

1. 计入 `input_edges_total`
2. 查它的 `flow_id` 在 `provider_map` 里有几个 provider
3. 按 provider 数量进入三种分支：

- `0` 个 provider：
  - 记为 `unmatched_no_provider`
  - 不写入 `A`
- `1` 个 provider：
  - 记为 `matched_unique_provider`
  - 直接写入 `A`
- `>1` 个 provider：
  - 记为 `matched_multi_provider`
  - 进入 `resolve_multi_provider(...)`
  - 若成功得到 resolution，则记为 `matched_multi_resolved`
  - 若 resolution 为 `None`，则记为 `matched_multi_unresolved`

### 2.4 当前支持的 provider rule

当前代码支持 5 种 provider rule：

- `strict_unique_provider`
- `best_provider_strict`
- `split_by_evidence`
- `split_by_evidence_hybrid`
- `split_equal`

它们的行为如下。

#### `strict_unique_provider`

语义：

- 只有当 provider 候选数恰好为 `1` 时才接受
- 只要 provider 候选数大于 `1`，直接 unresolved

实现上：

- 多 provider 分支返回 `Ok(None)`

结果：

- 所有 multi-provider case 都不会自动解析

#### `best_provider_strict`

语义：

- 对 multi-provider 候选打分
- 只选唯一 top1
- 但要求 top1 足够强，且必须显著优于 top2

判定门槛：

- 候选分数必须 `>= 0.35`
- top1 分数必须 `>= 0.55`
- `top1 / top2 >= 1.2`

如果不满足，就判失败。

#### `split_by_evidence`

语义：

- 对所有 `score >= 0.35` 的 provider 按证据分数加权分配
- 没有合格候选则失败

结果：

- 可以把一条 input edge 分配给多个 provider

#### `split_by_evidence_hybrid`

语义：

- 先尝试 `split_by_evidence`
- 如果没有合格候选，回退到 `split_equal`

结果：

- 基本可以把绝大多数 multi-provider case 变成可写入 `A` 的边

#### `split_equal`

语义：

- 完全不看打分
- 所有候选 provider 平均分配

结果：

- 所有 multi-provider case 都可落边
- 但语义最弱

### 2.5 当前打分特征

当前多 provider 自动解析只看三个特征：

- `model_id`
- `location`
- `reference_year`

处理顺序：

1. 如果 consumer 有 `model_id`，优先保留同 `model_id` 的 provider 子集
2. 如果同 `model_id` 子集为空，回退到全部 provider
3. 对候选按 geography 和 time 打分

#### geography score

- 同国家同州：`1.00`
- 同国家：`0.85`
- 同区域：`0.60`
- provider=`GLO`：`0.40`
- 其他：`0.10`

#### time score

若 `d = |provider_year - consumer_year|`：

- `d <= 1`：`1.00`
- `d <= 3`：`0.85`
- `d <= 5`：`0.65`
- `d <= 10`：`0.40`
- `d > 10`：`0.20`
- 任一侧缺 year：`0.50`

#### final score

- `final_score = 0.7 * geo + 0.3 * time`

### 2.6 当前会落下哪些诊断

当前 snapshot coverage 已输出这些指标：

- `input_edges_total`
- `matched_unique_provider`
- `matched_multi_provider`
- `matched_multi_resolved`
- `matched_multi_unresolved`
- `matched_multi_fallback_equal`
- `unmatched_no_provider`
- `a_input_edges_written`
- `unique_provider_match_pct`
- `any_provider_match_pct`

但当前还**没有**把 provider 级证据细节持久化为可复查的数据，例如：

- `evidence_vector`
- `resolution_confidence`
- `ambiguity_flag`
- top1/top2 竞争关系

因此当前只能从 coverage 聚合指标推断问题，不能直接从持久化决策明细回放每条 unresolved 的具体原因。

---

## 3. 当前线上状态

### 3.1 当前线上使用的规则

截至 2026-03-28，本次分析确认的最新 active snapshot：

- `snapshot_id = 7c6b394b-9fa2-4890-8160-fe1cccf8aaa4`
- `provider_matching_rule = split_by_evidence_hybrid`

这意味着：

- 当前线上 active snapshot 在 multi-provider 情况下已经启用自动解析
- `strict_unique_provider` 不再是当前线上 active snapshot 的行为
- 文档后文里出现的 `strict_unique_provider` 高 unresolved 指标，应理解为“历史基线 / replay 对照样本”

对应地：

- 旧基线样本仍然有分析价值
- 但它已经不再代表当前线上 active snapshot 的真实状态

### 3.2 当前 active snapshot 的 provider / A 指标

最新 active snapshot 的关键指标：

- `input_edges_total = 12005`
- `matched_multi_provider = 5814`
- `matched_unique_provider = 1698`
- `matched_multi_resolved = 5814`
- `matched_multi_fallback_equal = 235`
- `matched_multi_unresolved = 0`
- `unmatched_no_provider = 4493`
- `a_input_edges_written = 7512`
- `a_write_pct = 62.57%`
- `any_provider_match_pct = 62.57%`
- `provider_present_resolved_pct = 100.0%`

这里最重要的信号是：

- `matched_multi_unresolved = 0`
- `provider_present_resolved_pct = 100.0%`
- `a_input_edges_written = matched_unique_provider + matched_multi_resolved = 7512`

这说明当前线上 active snapshot 在 “只要存在 provider，就把 input edge 合理落进 `A`” 这个目标上，已经基本打通。

当前 `A` 写入率不再主要受 multi-provider unresolved 限制，而主要受下面两类因素限制：

- `unmatched_no_provider = 4493`：根本没有 provider 候选
- hybrid 规则虽可落边，但仍可能引入新的网络结构问题，例如 service-loop / provider-loop

也就是说，当前的核心问题已经从“多 provider 解析不出来”转向：

- 没有 provider 的边怎么处理
- 已解析出来的边是否会带来不合理网络结构
- 数据语义是否会放大 LCIA 异常

### 3.3 当前 active snapshot 与上一版 hybrid ready snapshot 基本一致

上一版可对照的 hybrid ready snapshot：

- `snapshot_id = e33fb379-ff31-46f3-8dec-271a55486688`

它在 provider / A 指标上和当前 active snapshot 基本一致：

- `input_edges_total = 12029`
- `matched_multi_provider = 5825`
- `matched_multi_resolved = 5825`
- `matched_multi_unresolved = 0`
- `matched_multi_fallback_equal = 240`
- `a_input_edges_written = 7530`
- `a_write_pct = 62.60%`
- `unmatched_no_provider = 4499`

说明当前 active snapshot 的 provider link 修复不是偶然波动，而是已经在最近两版 hybrid snapshot 上稳定出现：

- multi-provider unresolved 已经被压到 `0`
- provider-present case 已经达到 `100% resolve`
- equal fallback 占比处于低个位数水平（约 `4%`）

---

## 4. 对历史 strict 基线的进一步分析

下面的分析基于历史 strict 基线 snapshot `12f9fbca-9a64-48a5-abbb-8333456fe929`，并复现了当时 replay 中的打分逻辑。

### 4.1 先判断：是不是只是因为规则太保守

multi-provider 输入边总数：

- `5815`

如果仍使用当时的 strict 基线规则：

- `strict_unique_provider`
- 多 provider 全部 unresolved

如果改用其他规则，理论解开比例是：

- `best_provider_strict`：约 `10.13%`
- `split_by_evidence`：约 `95.96%`
- `split_by_evidence_hybrid`：几乎可覆盖全部，约 `4.04%` 会走 equal fallback

这个小节的结论应理解为“为什么当时必须切规则”，而不是“当前线上仍然 unresolved 高企”。

结论：

- unresolved 高企，第一层原因确实是当前线上规则太保守
- 但问题不止配置
- 因为即使切到 `best_provider_strict`，大部分 unresolved 仍然解不开

### 4.2 `best_provider_strict` 为什么解不开

对 `best_provider_strict` 的失败原因拆解如下：

- `top1/top2 ratio too close`：`4816`
- `top1 score below threshold`：`175`
- `no candidate >= min_score`：`235`

结论：

- 主要不是“候选太差”
- 主要是“候选太像，无法选出显著优于第二名的唯一 top1”

### 4.3 “候选太像”具体表现在哪里

在 `top1/top2 ratio too close` 的 case 里：

- `68.6%` 是 exact tie

按 geo/time 关系拆分：

- `same_geo_same_time`：`3242`
- `same_geo_diff_time`：`1288`
- `diff_geo_same_time`：`148`
- `diff_geo_diff_time_but_close`：`138`

结论：

- 大多数冲突并不是 location/year 缺失
- 而是多个 provider 在 location/year 上本来就几乎一样
- 当前特征粒度不足以区分它们

### 4.4 `model_id` 目前几乎没帮上忙

在 multi-provider 输入边里：

- `consumer_model_missing_pct = 92.47%`
- `same_model_pref_possible_pct = 0.24%`

结论：

- 当前 “先同 model_id 过滤” 这一步在生产样本里几乎没有实际效果
- 原因可能是：
  - consumer 大多没有 `model_id`
  - provider 侧没有形成可用的同 model 子集
  - 或者当前数据建模没有把 `model_id` 当作 provider link 的有效主键

### 4.5 location/year 不是主要缺失项

在 multi-provider 输入边里：

- `consumer_location_missing_pct = 0.0%`
- `consumer_year_missing_pct = 9.72%`
- `candidate_provider_all_location_missing_pct = 0.0%`
- `candidate_provider_all_year_missing_pct = 0.33%`

结论：

- location 数据整体不缺
- year 有一些缺口，但不是主要问题
- 真正问题是：即便 location/year 都有，它们也不足以把候选 provider 拉开

### 4.6 候选集合本身偏粗

provider 候选数分布（多 provider 输入边）显示，很多 flow 的候选池很大：

- `30` 个 provider：`1103` 条 input edge
- `18` 个 provider：`210` 条 input edge
- `25` 个 provider：`137` 条 input edge
- `66` 个 provider：出现在 top unresolved flow 中

说明：

- 当前 provider 池很多时候不是 “2 选 1”，而是 “几十选一”
- 当候选集合已经这么宽时，只靠 `location + year` 很难选出稳定唯一解

---

## 5. 核心问题归纳

### 问题 1：线上默认规则过于保守

这组旧基线样本里，线上使用的是 `strict_unique_provider`；当前代码默认值已切到 `split_by_evidence_hybrid`。

它的结果是：

- 唯一 provider 才能进 `A`
- 任何多 provider case 一律 unresolved

这会导致两个后果：

- `matched_multi_unresolved` 规模被放大
- `A` 写入率被人为压低到唯一 provider 的水平

这不是 bug，但会让系统长期停在“结构可计算但网络很薄”的状态。

### 问题 2：候选 provider 集合生成过粗

当前 provider 候选集只按 `flow_id` 聚合。

这会把很多实际上语义不同、但共享同一 flow_id 的 process 全部拉到同一个 provider 池里，导致：

- 候选数过大
- provider 竞争关系失真
- 后续评分难以选出唯一 top1

### 问题 3：自动解析特征不足

当前自动解析只看：

- `model_id`
- `location`
- `reference_year`

这三个特征对“高度标准化、同地区、同年代”的 provider 区分度不够。

结果是：

- 很多候选分数相同
- 大量 case 卡在 `top1/top2` 太接近

### 问题 4：`model_id` 过滤在生产里几乎不起作用

虽然代码里有“优先同 `model_id`”这层，但在线上样本里几乎没有起到筛选作用。

这说明：

- 当前数据里的 `model_id` 还没有形成有效 provider linking 信号

### 问题 5：缺少 provider 决策明细持久化

当前 coverage 只有聚合指标，没有 provider 决策明细。

缺失的能力包括：

- 每条 multi-provider edge 的候选列表
- 每个候选的 geo/time/final_score
- 为什么 unresolved
- 为什么 fallback

这会让后续优化成本很高，因为每次都要离线重放逻辑。

---

## 6. 修改策略

建议按四层推进，而不是一次性大改。

### 第一层：先把“看得见”补齐

目标：

- 让 unresolved 不再只是一个总数
- 让每条 unresolved 可以被解释

建议：

1. 在 snapshot build 过程中落 provider decision 明细
2. 至少持久化：
   - `candidate_provider_count`
   - `matched_provider_count`
   - `used_equal_fallback`
   - `top1_score`
   - `top2_score`
   - `top1/top2_ratio`
   - `geo_score`
   - `time_score`
   - `resolution_mode`
   - `ambiguity_flag`
   - `failure_reason`
3. 对 unresolved 输出分类统计：
   - `strict_rule_blocked`
   - `no_candidate_ge_min_score`
   - `top1_below_threshold`
   - `top1_top2_too_close`

这是后续所有调整的前提。

### 第二层：把生产默认策略从“只接受唯一 provider”升级为“可解释地自动分配”

目标：

- 先把 `A` 写入率从约 `14%` 提升到明显更高水平

建议：

1. 不建议直接把默认策略改成 `best_provider_strict`
   - 因为按当前样本，它只能解决约 `10%` 的 multi-provider case
2. 更适合作为生产候选的是：
   - `split_by_evidence_hybrid`
3. 推荐推进顺序：
   - 先离线对同一 snapshot 同时跑：
     - `strict_unique_provider`
     - `best_provider_strict`
     - `split_by_evidence`
     - `split_by_evidence_hybrid`
   - 比较：
     - `A` 写入率
     - solve 成功率
     - 热点 process 排名差异
     - 结果值域和异常值
4. replay 结果已经支持把 `split_by_evidence_hybrid` 提升为当前默认策略；后续重点转为监控 fallback 占比与结果稳定性

### 第三层：缩窄 provider 候选集合

目标：

- 让 provider link 的问题不再是“几十选一”

建议优先考虑引入更细的候选过滤维度，例如：

- reference product / quantitative reference 语义
- exchange 层更明确的产品/功能标记
- process 类型或 dataset subtype
- 单位兼容性
- 市场 process vs 技术 process 的显式区分
- provider flow 的更细 category / property 约束

原则：

- 能在候选生成阶段缩窄的，不要都留到打分阶段解决

### 第四层：补强自动解析特征

目标：

- 让“同 geo、同 time”的 provider 也能被进一步区分

建议新增或评估这些信号：

- flow / process 的行业或技术标签
- process name / classification 中的技术类型线索
- market / production / mix 语义
- 数据质量等级或推荐级别
- reference product 层的功能一致性
- 历史人工选择或规则白名单

如果这些信号还没有结构化字段，可以先在明细表里加 “evidence slots”，后面逐步接入。

---

## 7. 建议的执行顺序

### 阶段 A：先补观测

1. 落 provider decision 明细
2. 给 unresolved 增加 failure reason 分类
3. 固定 daily / weekly 指标：
   - `provider_usable_pct`
   - `a_write_pct`
   - `matched_multi_unresolved`
   - `unmatched_no_provider`

### 阶段 B：先做离线回放，确认默认切换依据

1. 对最新 active snapshot 跑四种 provider rule
2. 比较：
   - `A` 写入率
   - solve 结果差异
   - 热点 process 稳定性
   - 异常值分布

当前结果：

- `strict_unique_provider`：`a_write_pct = 14.14%`
- `best_provider_strict`：`a_write_pct = 19.05%`
- `split_by_evidence`：`a_write_pct = 60.62%`
- `split_by_evidence_hybrid`：`a_write_pct = 62.57%`，provider-present case `100%` resolve，`235` 条走 equal fallback

### 阶段 C：默认切到 `split_by_evidence_hybrid`

原因：

- `best_provider_strict` 改善有限
- `split_by_evidence_hybrid` 才能真正释放 multi-provider 网络
- 相比 `split_equal`，它保留了“优先按证据分配、仅在证据不足时兜底 equal”的解释路径

### 阶段 D：治理候选池

1. 先做 top unresolved flows 排查
2. 重点关注 provider 候选数极高的 flow
3. 为这些 flow 引入更细的候选过滤约束

---

## 8. 当前最重要的结论

一句话总结：

当前 provider link 的核心矛盾，已经不再是“multi-provider 解不开”，而是：

- multi-provider link 已经随着 `split_by_evidence_hybrid` active snapshot 基本打通
- 但候选 provider 集合仍然按 `flow_id` 聚合得较粗
- hybrid 解析后的边会把更多服务类 process 真正接入网络
- 这又暴露出 service-loop / provider-loop 和数据语义异常等下一层问题

因此，当前最值得优先推进的不是继续讨论 solve 端，而是把下面这条链路打透：

`input edge -> provider candidate set -> provider evidence -> provider resolution -> A write`

当前这条链路在 “provider-present case 是否能落边” 上已经取得明显改善；下一步更重要的是：

- 让每条已落边的 provider resolution 更可解释
- 识别并治理 service-loop / self-loop / transport service 环
- 识别并治理 `PN -> Mass PM0.2` 这类会直接污染 LCIA 的数据语义问题

换句话说：

- multi-provider link 问题前一阶段已经基本修复
- 当前阶段的重点，已经转向“网络质量”和“结果质量”
