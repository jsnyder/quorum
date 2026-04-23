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

fn safe_try_current() {
    // no-match: degrades on no-runtime
    let _ = Handle::try_current()
        .ok()
        .and_then(|h| h.block_on(async { 42 }).into());
}

fn safe_block_in_place_on_multi_thread() {
    // no-match: block_in_place wraps the block_on, which is the
    // documented multi-thread-safe pattern.
    let handle = Handle::try_current().expect("runtime");
    let _ = tokio::task::block_in_place(|| handle.block_on(async { 42 }));
}
