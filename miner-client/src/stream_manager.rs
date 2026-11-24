//! CUDA scheduler lives in cuda/stream_manager.cu and builds independently.
//! Do not advertise background-allocated memory as free: the router uses actual
//! NVML free bytes. Reclaimable memory needs a separate, verified capacity field.
//! A production FFI adapter must serialize owner-thread commands and expose task
//! completion/errors; no unsafe cross-context FFI is silently enabled here.
