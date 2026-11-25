# Indexed precise-range readback

The GPU-written-page path used to scan every declared root-UAV range for
each marked page. Build a sorted start/prefix-maximum-end index once per
readback and binary-search it for each page. The old predicate and
page-level precedence are preserved for all representable ranges,
including nested, repeated, adjacent and zero-length declarations.
Overflowing ends saturate rather than wrapping or panicking.

Complexity for R ranges and P candidate pages changes from worst-case
O(P*R) to O(R log R + P log R), with O(R) additional temporary storage.
Small workloads may not benefit. Sorting and index allocation are not
included in lookup operation counts; real CPU and frame times require
release benchmarks. No change to GPU commands, page tables, bitmap
ownership, or copy-back policy. Complements #1865 rather than duplicating
its bitmap or page-table changes.

Tests:
    cargo test -p victory-gpu memory::precise_ranges --locked
    cargo test -p victory-gpu memory --locked
    cargo clippy -p victory-gpu --all-targets --locked -- -D warnings
    cargo xtask policy
