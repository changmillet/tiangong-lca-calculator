---
title: Implicit Regional Supply Mix Modeling
docType: theory
scope: repo
status: active
authoritative: false
owner: worker
language: en
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
---

# Modeling an implicit regional supply mix

[中文](implicit-regional-supply-mix-modeling.md) · [Provider linking runtime contract](provider-linking.en.md)

A Model lets a modeler specify which processes supply particular exchanges. The resulting inventory can still contain exchanges that need an external supply or treatment process. For those exchanges, Worker uses the available Process data to build a calculable product system. When several providers qualify, the **implicit regional supply mix** estimates their shares from geography and annual volume. The calculation creates no separate market Process.

## Explicit supply relationships and the primary result Process

Connect an output of the upstream Process instance to the corresponding input of the downstream instance to specify who supplies that delivery. A Model can contain several Processes. The connections between their exchanges identify the actual modeled deliveries.

When the platform calculates and saves the Model for its functional unit, the primary product result includes the inventory contributions of the connected processes. Internal deliveries cancel against their corresponding consumption. Under the single-primary policy, creating a Model or a new version exposes one primary result Process. Historical multi-result versions retain their existing result references. The calculation still applies coproduct allocation, and the standard calculation graph can reference any allocated processes it needs. If the primary result has nonzero product or waste exchanges other than its quantitative reference that still need external balancing, its dataset type is `Partly terminated system`. Once these exchanges are closed, the primary result can be represented as an `LCI result`.

A downstream user who references the primary result inherits the supply relationships already accounted for in its inventory. Worker looks for providers only for exchanges that remain open. A source Process and a primary result that includes it may both qualify to supply a separate exchange. Their Model provenance is available for inspection. Sharing a Model gives neither candidate priority under the default rule.

## Determine the quantity attributable to the reference product

Product allocation first determines how much of an exchange belongs to the current reference product. Worker multiplies the quantity by its direction sign (`−1` for an input, `+1` for an output) and normalizes by the absolute quantity of the reference exchange. For a non-reference exchange it also applies the selected product allocation fraction, giving the residual coefficient `c_r`. The reference exchange itself retains its normalized sign `c_i ∈ {−1, +1}`.

The exchange direction and the sign of the recorded quantity both matter. Their product determines the sign of the residual. A provider must balance that sign even when its quantitative reference is a treatment input.

## Which exchanges can link

Worker considers each product or waste exchange in the technosphere. An eligible Process must be available in the calculation scope. Its quantitative reference exchange must match the exact Flow revision, have compatible units and have a normalized coefficient of the opposite sign to the residual. A coproduct listed under the same name elsewhere in a Process inventory does not by itself qualify that Process as an independent provider of the coproduct. Elementary flows enter the environmental inventory. Other flows are retained for reporting only.

With no eligible reference exchange, the residual remains open and the snapshot boundary policy determines how to handle it. One eligible provider covers the whole residual. With several, Worker selects the geographic tier and assigns shares within it. To publish a particular market mix or cite it across studies, model it explicitly as a Process.

## Supply region and annual volume

**Select the geographic tier first.** A modeler can give an exchange its own supply region through `exchange.location`. Worker uses that location when available and otherwise uses the location of the Process containing the exchange. If neither is usable, the supply region is unspecified. Worker selects the first tier with eligible providers in this order: same subnational area, same country, same region, global, other. Larger volumes in other tiers cannot affect the shares for this exchange. Geographic preference is an assumption about supply. Evidence of actual procurement must come from the study.

**Calculate shares within that tier.** For candidate `i`, a finite positive value read from its annual supply or production volume (`annualSupplyOrProductionVolume`) supplies the raw weight `v_i`. The share is `w_i = v_i / Σv`. Missing, unreadable, nonpositive or nonfinite values receive a weight of `1`, and diagnostics count each fallback. The fallback keeps a candidate in the mix. Its actual production volume remains unknown. If every volume is missing, shares are equal. If some are missing, the fallback weights are normalized together with the observed values.

Interpreting these shares as supply proportions requires comparable units, periods and reporting boundaries. Worker reads the numeric value from the text without converting annual-volume unit suffixes. The unit compatibility check on reference exchanges does not validate the basis of annual-volume figures. A source Process and an aggregate result may both qualify even when they cite production from the same line. Using their shares as market estimates requires checking for overlapping production figures.

## How supply shares balance an exchange

The supply share `w_i` distributes this already determined residual. The required activity `a_i` of each provider satisfies

```text
w_i >= 0                        sum(w_i) = 1
a_i = (-c_r / c_i) * w_i        c_r + sum(c_i * a_i) = 0
```

Changing the mix can change which processes are used and their environmental impacts while preserving the quantity balanced for that exchange. For explicit targets, sparse zero shares and historical output-share formats, see the [runtime rules](provider-linking.en.md#product-allocation-and-historical-formats).

## Unresolved exchanges and interpretation

“Partly terminated system” describes the boundary of the saved Process inventory. A downstream calculation can use that Process under `closed`. If all its open exchanges link, it satisfies this closure requirement. An unresolved residual blocks readiness under the ordinary snapshot default of `closed`. The `open` policy allows an unresolved exchange to remain at the boundary, and `cutoff` permits truncation at a declared boundary. Both retain warnings and per-exchange evidence. Certificate-grade Scope Closure freezes `cutoff`, so a missing provider produces diagnostic evidence and does not by itself block the certificate.

To interpret a result, establish which supply relationships the Model specified. For automatic links, inspect the Flow revision, candidate Processes, source of the supply region, selected tier, volume weights and fallbacks, final shares and unresolved quantities. A successful link establishes a way to balance the exchange. If the study specifies a supplier, connect the corresponding Process in the Model. If the study calls for regional average supply, check the vintage, technology and market coverage of the selected Processes before treating their shares as representative.
