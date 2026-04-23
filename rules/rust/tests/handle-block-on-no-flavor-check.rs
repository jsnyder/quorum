// Fixture: handle-block-on-no-flavor-check
//
// Pattern from issues #57 / #58 / #71: Handle::current() panics with no
// runtime, and Handle::block_on panics inside an async context. The safe
// pattern is try_current().ok() + a RuntimeFlavor check.

use tokio::runtime::Handle;

fn buggy_qualified() {
    // match: fully qualified path
    let _ = tokio::runtime::Handle::current().block_on(async { 42 });
}

fn buggy_use() {
    // match: imported short form
    let _ = Handle::current().block_on(async { 42 });
}

fn safe_with_flavor_match() {
    // no-match: try_current() handles no-runtime, RuntimeFlavor match
    // dispatches block_in_place on multi-thread (safe in async context)
    // vs a separate-thread fallback on current-thread. This is the only
    // pattern that's safe at any call site (the block_on_async impl in
    // src/llm_client.rs uses this shape).
    use tokio::runtime::RuntimeFlavor;
    let _ = match Handle::try_current() {
        Ok(handle) => match handle.runtime_flavor() {
            RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(async { 42 }))
            }
            _ => std::thread::scope(|s| {
                s.spawn(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build fallback runtime");
                    rt.block_on(async { 42 })
                })
                .join()
                .expect("fallback thread panicked")
            }),
        },
        Err(_) => 42,
    };
}
