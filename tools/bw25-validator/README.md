# bw25-validator

Manual-only Brightway25 cross-check tool for this repository.

It validates one result (`solve_one` or `solve_all_unit`) against a Brightway reconstruction from the same snapshot artifact:

- input: `lca_snapshot_artifacts` (`snapshot-hdf5:v1`) + `lca_results` (`hdf5:v1`, S3 artifact only)
  - both artifact formats support HDF5 built-in `deflate` compression transparently
- compute: Brightway `LCA(..., data_objs=[datapackage])` on technosphere matrix `M`
- compare: `x`, `g`, `h` vectors + residuals
  - `solve_all_unit`: iterates unit demand per process (`e_i`), compares each item and aggregates worst-case metrics
- speed compare: prefer Rust comparable compute time (`solve_mx + bx + cg`) against Brightway solve/build+solve timings
- output: JSON + Markdown report in `reports/bw25-validation/`

This tool is not part of the default worker path. Run it manually.

On Linux `x86_64`, the tool installs `pypardiso` by default to speed up sparse solves and avoid Brightway's x64 warning about missing `pypardiso`.
