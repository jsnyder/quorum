// Fixture: unwrap-after-infallible

fn safe_unwraps() {
    // match: Some(x).unwrap() is always safe
    let val = Some(42).unwrap();

    // match: Ok(x).unwrap() is always safe
    let ok = Ok("hello").unwrap();

    // match: Some with expression
    let computed = Some(vec![1, 2, 3]).unwrap();
}

fn unsafe_unwraps() {
    // no-match: unknown option, could be None
    let maybe: Option<i32> = get_value();
    let val = maybe.unwrap();

    // no-match: result from fallible operation
    let data = std::fs::read("file.txt").unwrap();

    // no-match: method call result
    let item = map.get("key").unwrap();
}
