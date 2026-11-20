---
title: Implicit Regional Supply Mix Modeling
docType: theory
scope: repo
status: active
authoritative: false
owner: worker
language: zh-CN
whenToUse:
  - when reasoning about signed-flow routing based on process annual supply or production volume
  - when reasoning about explicit exchange location as a supply-region anchor
  - when evaluating whether a regional technosphere mix can be represented without an explicit market process
  - when implementing or reviewing calculator routing weights
whenToUpdate:
  - when signed-flow candidate or routing semantics change
  - when exchange location supply-region semantics change
  - when annual supply or production volume parsing semantics change
  - when calculator starts materializing explicit market processes instead of implicit direct links
checkPaths:
  - docs/provider-linking.md
  - docs/provider-linking.en.md
  - docs/implicit-regional-supply-mix-modeling.md
  - docs/implicit-regional-supply-mix-modeling.en.md
  - crates/solver-worker/src/bin/snapshot_builder.rs
  - crates/solver-worker/src/compiled_graph.rs
  - crates/solver-worker/src/signed_flow.rs
  - crates/solver-worker/src/snapshot_artifacts.rs
lastReviewedAt: 2026-11-20
lastReviewedCommit: eb84fc9ba7cfea3f41124ed00647617e36e87941
lastReviewedNote: "Worker #381: checked explicit Model supply, open exchanges, signed eligibility, volume interpretation and historical allocation against the PR implementation."
related:
  - AGENTS.md
  - docs/agents/repo-architecture.md
  - docs/agents/repo-validation.md
  - docs/provider-linking.md
  - docs/provider-linking.en.md
  - docs/lca-api-contract.md
  - docs/implicit-regional-supply-mix-modeling.en.md
---

# 隐式区域供应组合的建模方法

[English](implicit-regional-supply-mix-modeling.en.md) · [供应者链接运行规则](provider-linking.md)

建模者可以在 Model 中指定一笔交换由哪个过程供应。汇总后的清单仍可能有需要外部供应或处理的交换。对这些开放交换，Worker 用已有 Process 数据构建可计算的产品系统。当多个供应者都合格时，**隐式区域供应组合**根据地域和年产量估算各自的份额。这个组合只存在于本次计算中，无须额外创建市场 Process。

## 显式供应关系与主结果 Process

在 Model 中，将上游过程实例的输出连接到下游实例的对应输入，即可指定这笔交付由哪个过程承担。同一个 Model 可以包含多个过程，具体的供应关系由交换之间的连线表达。

按功能单位计算并保存时，平台将已连接过程的清单贡献汇入主产品结果，内部交付量与对应的消费量在汇总中抵消。在单主结果策略下，新建 Model 或创建新版本时只对外提供一个主结果 Process。历史多结果版本保留原有结果引用。多产品分配仍然参与计算，标准计算图仍可引用所需的分配后过程。主结果除定量参考交换外，若仍有非零的产品或废物交换需要外部平衡，其类型为**部分终止系统**（`Partly terminated system`）；这些交换全部闭合时，主结果可表示为 `LCI result`。

下游引用主结果时，已经计入清单的供应关系随结果一起使用。Worker 只为剩余的开放交换查找供应者。一份原始过程和一份包含它的主结果如果都符合另一笔交换的供应条件，两者都可进入候选。模型来源供追溯；在默认规则下，同属一个 Model 不带来供应优先权。

## 确定归属于参考产品的待平衡量

产品分配先决定一笔交换有多少归属于当前参考产品。Worker 将交换数量乘以方向符号（输入为 `−1`，输出为 `+1`），再按参考交换数量的绝对值归一化。非参考交换还要乘以所选产品的分配比例，得到待平衡系数 `c_r`。参考交换自身保留归一化符号 `c_i ∈ {−1, +1}`。

交换方向和原始数量各自带有符号，两者相乘决定待平衡量的符号。对以废物输入为定量参考的处置过程，也用参考系数的符号判断它能平衡哪一笔交换。

## 哪些交换可以链接

Worker 逐笔处理技术系统中的产品流和废物流。一个候选过程要能平衡这笔交换，必须在本次计算范围内可用，其定量参考交换须匹配同一 Flow 的确切版本、具有兼容单位，且归一化系数与待平衡量符号相反。过程清单里出现某个同名副产品，本身不足以将该过程作为这个副产品的独立供应者。基本流进入环境清单，其他类型的流仅保留报告记录。

没有合格的参考交换时，这笔量仍然开放，按快照的边界策略处理。只有一个合格供应者时，它承担全部待平衡量。有多个时，Worker 先选择地域层级，再分配层内份额。如需发布一份特定的市场组合或在多个研究中引用它，可以显式建成一个 Process。

## 供应区域与年产量

**先选地域层级。** 建模者可以用 `exchange.location` 为一笔交换指定供应区域。该地点可用时优先采用，缺失或不可用时才用该交换所属过程的所在地；两者均不可用时，供应区域记为未指定。Worker 按同一地方行政区、同一国家、同一区域、全球、其他的顺序，选择第一个有合格候选的层级。选定后，其他层级的年产量再大也不会改变这笔交换的供应份额。这里的地域优先表达的是对供应来源的假设，具体采购地仍需研究证据支持。

**再计算层内份额。** 对选中层级中的候选 `i`，Worker 从其年供应量或年产量字段（`annualSupplyOrProductionVolume`）读取有效的有限正数作为原始权重 `v_i`，并取 `w_i = v_i / Σv`。缺失、无法解析、非正或非有限值使用权重 `1`，诊断记录使用次数。回退值让候选仍能参与分配，其实际年产量仍然未知。全部缺失时各候选均分；部分缺失时，默认权重与已读到的数值一起归一化。

将这些份额解释为供应比例，前提是产量使用可比单位、期间和统计口径。Worker 读取文本中的数值，不根据单位后缀换算年产量。参考交换的单位兼容检查也不验证年产量口径。若原始过程与汇总结果引用同一生产线的产量，二者可共同参与链接；用它们估算市场供应份额时，还须核实所代表的产量是否重叠。

## 供应份额如何平衡一笔交换

供应份额 `w_i` 只拆分已经确定的待平衡量。各供应者所需的活动量 `a_i` 满足：

```text
w_i >= 0                        sum(w_i) = 1
a_i = (-c_r / c_i) * w_i        c_r + sum(c_i * a_i) = 0
```

因此，改变供应组合可以改变承担活动的过程及其环境影响，同时保持这笔交换的总量平衡。显式分配目标、稀疏零份额和历史输出份额格式的处理见[运行规则](provider-linking.md#产品分配与历史格式)。

## 未闭合交换与结果解释

“部分终止系统”描述保存下来的 Process 清单边界。下游计算可以按 `closed` 策略使用这份 Process：只要其开放交换都能链接，就满足这项闭合要求。若仍有待平衡量，普通快照默认的 `closed` 策略会阻断就绪检查；`open` 允许保留未闭合交换，`cutoff` 允许按声明的边界截断。后两者均须保留警告及逐笔记录。用于完整性认证的 Scope Closure 工作流固定采用 `cutoff`，缺少供应者会留下诊断记录，单凭这一点不会阻断证书。

解释结果时，先确认哪些供应关系已由 Model 指定，再对自动链接核对 Flow 版本、候选过程、供应区域的来源和选中层级、产量权重及其回退、最终份额和未闭合量。链接成功说明计算找到了平衡这笔交换的方式。若研究指定了供应商，可以在 Model 中连接相应过程。若研究采用区域平均供应，则应核对选中过程的年份、技术和市场覆盖程度，再判断所得份额能否代表研究对象。
