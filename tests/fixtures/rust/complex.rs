// Expected: complexity finding for process(). Cyclomatic complexity must
// stay >= 2 * threshold (default threshold = 10) so the analyzer emits a
// Medium-severity finding under the post-PR-50 cap, which keeps the
// `review_complex_file_exits_nonzero` integration test meaningful.
fn process(a: bool, b: bool, c: bool, d: bool, e: bool, f: bool, g: bool) {
    if a {
        if b {
            if c {
                if d {
                    if e {
                        return;
                    }
                }
            }
        }
    }
    if a && b || c {
        return;
    }
    if d && e || f {
        return;
    }
    if a || b && c {
        return;
    }
    if d || e && f {
        return;
    }
    if a && c && e {
        return;
    }
    if b || d || g {
        return;
    }
    for i in 0..10 {
        if i > 5 {
            break;
        }
        if i == 3 {
            continue;
        }
    }
    let mut guard = 0;
    while a && guard < 5 {
        if b {
            break;
        }
        guard += 1;
    }
}
