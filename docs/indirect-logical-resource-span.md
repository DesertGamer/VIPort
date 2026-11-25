# Logical bounds for indirect argument buffers

The command-list entry point resolves an argument or count resource before it passes a guest address to the renderer. A native shadow can cover more guest memory than that resource describes. With two placed resources at the same guest address, a scan of the available backing can select the larger shadow even when a requested record extends past the smaller resource. The smaller resource's descriptor is the correct size for the bounds check.

The guard checks the final argument payload and the four-byte count word. The offset, span, and final address must fit. The size calculation accepts a final record that ends immediately after its payload. Zero maximum records return before resolving buffer addresses or recording native work.

Thanks to raffclar for identifying the final-record span case and reviewing count-buffer state tracking in #40. This patch checks the guest resource before native argument preparation. Count-buffer state, COMMON promotion, shared argument/count resources, legacy barrier fallback and producer handoffs belong to the native clamp and ordering paths. #242 covers tight record spans and count-aware clamping, with its own native controls.

```powershell
cargo test -p victory-gpu --features diagnostics --locked indirect_span -- --include-ignored --test-threads=1
$env:VICTORY_D3D12_WARP = '1'
cargo test -p victory-gpu --features diagnostics --locked indirect_span -- --include-ignored --test-threads=1
```

The native-shadow test uses two original buffers at the same address. It confirms that a 64-byte D3D12-backed shadow can cover a request that the 28-byte logical argument resource must refuse. It tests exact-end and one-word-out-of-range count offsets separately. The regression stops at resource selection and does not submit an out-of-range draw or dispatch. The fixture passed on local AMD and WARP without GPU-based validation.
