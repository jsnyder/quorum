// Expected: complexity finding for process()
fn process(a: bool, b: bool, c: bool, d: bool, e: bool) {
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
    for i in 0..10 {
        if i > 5 {
            break;
        }
    }
}
