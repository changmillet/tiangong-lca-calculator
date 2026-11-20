---
title: Provider Linking Runtime Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: zh-CN
whenToUse:
  - when explaining or changing calculator provider-link runtime behavior
  - when changing snapshot_builder provider rule defaults or candidate filtering
  - when reviewing provider decision diagnostics, A-write coverage, or provider closure
whenToUpdate:
  - when provider candidate eligibility changes
  - when provider rule defaults or resolution order changes
  - when provider decision diagnostics change
checkPaths:
  - docs/provider-linking.md
  - docs/provider-linking.en.md
  - docs/implicit-regional-supply-mix-modeling.md
  - docs/implicit-regional-supply-mix-modeling.en.md
  - docs/matrix-readiness-report-contract.md
  - crates/solver-worker/src/bin/snapshot_builder.rs
  - crates/solver-worker/src/compiled_graph.rs
  - crates/solver-worker/src/snapshot_artifacts.rs
lastReviewedAt: 2026-11-20
lastReviewedCommit: eb84fc9ba7cfea3f41124ed00647617e36e87941
lastReviewedNote: "Worker #381: checked explicit Model supply, open exchanges, signed eligibility, volume interpretation and historical allocation against the PR implementation."
related:
  - docs/provider-linking.en.md
  - AGENTS.md
  - docs/implicit-regional-supply-mix-modeling.md
  - docs/implicit-regional-supply-mix-modeling.en.md
  - docs/matrix-readiness-report-contract.md
---

# 供应者链接运行规则

[English](provider-linking.en.md) · [隐式区域供应组合的建模方法](implicit-regional-supply-mix-modeling.md)

Worker 在构建快照时逐笔链接技术系统交换，生成过程间的需求矩阵 `A`。求解阶段使用已经冻结的矩阵。对于 Model 主结果，建模者在图中指定的内部交付已随汇总抵消，相关过程的清单贡献已计入结果。下面的规则作用于结果边界上仍需供应或处理的交换，同样适用于普通 Process 的开放交换。

对每笔开放的产品或废物交换，快照构建时的默认链接策略按以下顺序执行。

```text
分配并归一化交换量 → 查找同一 Flow 版本、符号相反的参考交换
    ├─ 无候选：记录未闭合交换，按快照边界策略处理
    ├─ 一个：取全额，将所需活动量写入 A
    └─ 多个：先选地域层级，再按年产量分配，将各供应者的活动量写入 A
```

## 确定哪些交换需要链接

在本次计算范围内，一个完整 TIDAS Process 版本对应一个过程索引和一个矩阵列。`quantitativeReference.referenceToReferenceFlow` 必须唯一指向本列的定量参考交换，且其计算数量有限、非零。参考交换可以为输入或输出。列内其他交换不会产生独立矩阵列；要让副产品独立供应其他过程，须有以它为定量参考的完整 Process 版本。

Worker 按 `resultingAmount`、`meanAmount`、`meanValue` 的顺序取计算数量。输入的方向符号为 `−1`，输出为 `+1`。用原始参考系数的绝对值归一化后，参考系数为 `−1` 或 `+1`。其他交换还需应用归属于当前参考产品的分配比例。

```text
raw_coefficient = direction_sign * calculation_amount
reference_scale = 1 / abs(raw_reference_coefficient)
c_r = raw_coefficient * reference_scale * selected_allocation_fraction
c_i = sign(raw_reference_coefficient)
```

Flow 类型决定后续处理。产品流与废物流中，非零的非参考交换形成待平衡量 `c_r`；基本流写入环境清单矩阵 `B`；其他流仅保留报告记录。显式或稀疏的零分配不扩大请求根的依赖集合，不计入匹配统计，也不写入 `A` 或 `B`。分配字段和历史格式见[产品分配与历史格式](#产品分配与历史格式)。

## 查找合格的参考交换

普通公开计算只接纳 `state_code = 100` 的过程。允许本人草稿的范围还可接纳属于当前用户的 `0`；预留发布段 `101..199`（含已发布 Result `120`）不具有该数值资格。未进入本次范围的过程不会出现在候选拒绝记录中。

Worker 按 `(Flow UUID, resolved version)` 为定量参考交换建立索引。已声明的 `@version` 按精确版本解析；省略时，按快照可见范围确定一次版本并冻结。同一 UUID 的多个实际使用版本可以共存。候选须匹配该 Flow 身份、单位兼容，且参考系数 `c_i` 与 `c_r` 符号相反。一个过程仅在非参考交换中列出该 Flow，不足以供应该交换。产品、废物、输入、输出这些标签本身都不能确定平衡方向。

核对符号时，`Waste Input +1000` 与 `Waste Output -1000` 的原始系数均为 `−1000`，归一后均可平衡正的待平衡量。`Waste Output +1000` 归一后为 `+1`，可平衡负的待平衡量。同一过程自己的参考交换若符合条件也可参与，选中后写入对角项 `A[i,i]`。

## 选择供应者并确定份额

默认策略 `split_by_process_volume` 在收集依赖和编译矩阵时使用同一套候选规则。Worker 根据 `referenceToResultingProcess` 和 `processInstance.referenceToProcess` 记录结果过程与组成过程之间的精确版本关系，包括嵌套关系。请求根指定计算从哪个过程版本开始。对每笔开放交换，默认策略保留合格的结果过程、组成过程及其他供应者；同一合格过程版本经多条路径找到时只参与一次分配。同属一个 `model_id` 或具有来源关系的候选按相同规则竞争。旧版默认策略曾在地域选择之前优先采用同 Model 候选。

| 合格候选数 | 默认决策 |
| --- | --- |
| 0 | 同一 Flow 版本没有参考交换时记录 `no_provider_candidates`，有参考交换但符号均不匹配时记录 `no_opposite_sign_reference`，本次平衡不写入 `A`。未闭合交换按快照边界策略处理。 |
| 1 | 唯一供应者承担全部待平衡量，份额为 1。 |
| 多个 | 先选地域层级，再在选中层级内按年产量归一化份额。 |

1. **供应区域。** 优先使用该笔交换可用的 `exchange.location`，其次使用该过程可用的所在地，两者均不可用时记为未指定。
2. **地域层级。** 按同一地方行政区、同一国家、同一区域、全球、其他的顺序，取第一个非空层级。跨层级的产量不参与比较。
3. **年产量。** 从供应者的 `annualSupplyOrProductionVolume` 文本或多语言 `#text` 中读取首个可解析的有限正数作为原始权重。解析器取文本开头由空白分隔的数值，移除其中的逗号；单位后缀不触发换算。缺失、非法、非有限或非正值使用固定权重 `1`。全部回退时层内均分，部分回退时与有效数值一起归一化。诊断记录回退次数。解释份额时须核对单位、期间和统计口径，尤其注意不同 Process 身份可能引用同一份实际产量。

## 写入矩阵并检查边界

设 `w_i` 为归一化后的供应份额。产品分配已决定 `c_r` 的大小。供应组合决定由哪些过程承担这笔量，各份额之和为 1，活动需求非负。

```text
activity_requirement_i = (-c_r / c_i) * w_i
A[balancing_process_i, dependent_process] += activity_requirement_i
closure = c_r + sum(c_i * activity_requirement_i) = 0
M = I - A
```

`self_loop_cutoff` 仅用于诊断对角值风险。保留真实对角项后，求解器根据完整的 `M` 判断可解性。

保存时的部分终止系统 Process 可以用于任何适用的下游快照边界策略。`--technosphere-boundary-policy` 决定 Worker 如何对待本次计算仍未闭合的非零交换：

| 策略 | 就绪检查及证据 |
| --- | --- |
| `closed`（普通快照默认） | 要求全部闭合。未闭合的非零交换阻断就绪检查。 |
| `open` | 允许待平衡量保留在系统边界，记录警告和逐笔证据。 |
| `cutoff` | 允许按声明的边界截断该量，记录警告和逐笔证据。 |

未知策略触发错误。用于完整性认证的 Scope Closure 工作流由数据库将省略值及兼容的边界输入规范化为 `cutoff`，Worker 只接纳冻结后的 `cutoff`。未匹配交换、`A` 写入覆盖率和未闭合量仍需报告，单凭缺少供应者不会阻断该证书。

## 从诊断记录复核一笔交换

| 要复核的问题 | 在决策与平衡记录中核对什么 |
| --- | --- |
| 哪一笔量需要平衡？ | 按过程和交换标识找到 Flow UUID、版本和参考单位。用原始方向、数量、参考交换的归一化尺度及产品分配比例核对 `residual_coefficient`。 |
| 为什么某个过程可选？ | 核对候选的过程版本、参考交换及其系数符号，并查看被排除候选的拒绝原因。未闭合记录的 `required_reference_sign` 给出所需符号。候选的 `model_id`、`model_version` 和来源关系用于追溯。 |
| 为什么选了这些供应者？ | `candidate_provider_count` 是策略选择前符号匹配的候选过程数，`matched_provider_count` 是实际分到份额的过程数。用 `decision_kind` 区分唯一供应、多候选已解决、多候选未解决和无供应者，用 `resolution_strategy` 确认实际选择方法。再查供应区域来源 `supply_region_source`、选中层级 `geography_tier` 及产量回退次数 `volume_fallback_to_one_count`。 |
| 这笔量最终闭合了吗？ | 用每个供应者的 `routing_weight`、`reference_coefficient` 和 `activity_requirement` 核对上文的平衡式。若仍未闭合，查看 `failure_reason` 和本次快照的边界策略。 |

`residual_edges_total` 和 `a_balance_edges_written` 分别统计待平衡边和写入边，旧字段 `input_edges_total` 和 `a_input_edges_written` 用于兼容。诊断导出和就绪报告消费已编译的决策，以便从同一证据解释结果。[矩阵就绪报告](matrix-readiness-report-contract.md)

## 产品分配与历史格式

定量参考交换本身不乘以分配比例。对非参考交换，先识别过程使用的分配模式，再确定归属于当前参考产品的比例。

| 数据形态 | 处理 |
| --- | --- |
| 显式目标 | `allocations.allocation` 可为对象或数组。按 `@internalReferenceToCoProduct == referenceToReferenceFlow` 选择当前目标。`@allocatedFraction` 的字符串或数字按 TIDAS `Perc` 除以 100，标准格式不带百分号。非零项已闭合到 100% 而当前目标省略时，按稀疏零处理。 |
| 无分配声明 | 比例为 1。历史标量 `allocations.allocation = {}` 同样按未声明处理，并记录 `legacy_empty_allocation_as_undeclared_count`。 |
| 历史输出份额 | 不同输出各自声明的无目标份额构成合计 100% 的向量。当前参考输出的份额作用于所有非参考交换，参考交换未声明份额时沿用默认值 1。允许历史百分号后缀，输入不可混入份额。 |
| 历史单项全额分配 | 在没有历史输出向量和显式目标的情况下，输入或处置交换上的单项全额分配可推断给唯一可识别的定量参考交换。仅接受规范数值 100 或精确字符串 `"100%"`。记录 `legacy_single_reference_target_inferred_count`；旧名 `legacy_single_output_target_inferred_count` 为兼容投影。 |

显式目标须唯一指向输入或输出交换，按其声明的 UUID 和版本精确解析为产品流或废物流。缺版本、换版本、未知类型或基本流目标均拒绝，普通构建和审核叠加使用相同检查。显式与无目标声明混用、同一交换上有多项含义不明的无目标条目、空数组、`[{}]`、不完整对象、重复或未知目标、非有限或越界比例，以及总和不闭合的向量均拒绝。兼容处理保留源 JSON。

## 其他显式策略

使用其他策略时，Worker 先执行 `version-exact-lineage-gate-v1`。若有来源冲突且请求根唯一选中一个结果过程，该结果承担这笔需求；否则冲突记录为 `lineage_overlap_requires_binding`。下表的候选数指通过这一步的候选数。

| 策略 | 多个候选时的行为 |
| --- | --- |
| `strict_unique_provider` | 记录未解决。 |
| `best_provider_strict` | 有同 `model_id` 候选时先收窄，再按地域与时间评分，要求第一名唯一，且满足最低分和领先比例。 |
| `split_by_evidence` | 有同 `model_id` 候选时先收窄，达标者按证据得分分配。 |
| `split_by_evidence_hybrid` | 同样优先同 `model_id` 候选进行评分，证据不足时回退到均分。 |
| `split_equal` | 合格候选均分。 |

## 快照身份与数据依赖

默认链接策略在构建配置中记为 `provider_lineage_policy = volume-lineage-evidence-v2`。普通快照的数据指纹包含构建配置和所使用的 Model 内容摘要。快照同时记录边界策略和 Flow 身份策略。覆盖率格式为 `snapshot_coverage.v3`，就绪检查输入与报告使用 v2，计算包使用 `tiangong.calculation-bundle.v2`。历史矩阵保留原有选择。复核时读取该快照记录的策略和版本；新计算对已有快照的准入另见[计算身份契约](scope-closure-contract.md#calculation-implementation-identity)。各项配置的当前取值见[快照构建器](../crates/solver-worker/src/bin/snapshot_builder.rs)。

Worker 仅为最终过程依赖集合实际引用的 Flow 版本分配矩阵索引。LCIA 因子只在与基本流轴及适用方向相符时进入表征矩阵 `C`；有效目标必须解析为基本流。相关流属性、单位组、来源和联系人随所需依赖解析。表征因子不参与供应者选择。构建器批量读取相关 Model 版本，缓存嵌套组成关系供候选查询；耗时记录在 `fetch_provider_lineage_sec`。求解时直接使用冻结的稀疏矩阵。

分配规则也有独立的版本约束。采用 `tidas-reference-allocation-v5` 的新计算检查已存分配标识，证书范围与冻结的数据快照均须声明兼容版本。部署时需配套采用生成该标识的 Database 和 Edge 版本；历史分配产物仍可解码和读取。
