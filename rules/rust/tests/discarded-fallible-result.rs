// Fixture: discarded-fallible-result
use std::sync::Mutex;
use std::net::TcpStream;

fn bad_examples() {
    let m = Mutex::new(0);
    // match: discarded lock result
    let _ = m.lock();

    // match: discarded send on channel
    let _ = tx.send(42);

    // match: discarded flush
    let _ = writer.flush();

    // match: discarded shutdown
    let _ = stream.shutdown();
}

fn good_examples() {
    let m = Mutex::new(0);
    // no-match: result used
    let guard = m.lock().unwrap();

    // no-match: propagated
    tx.send(42)?;

    // no-match: non-fallible method
    let _ = vec.clone();

    // no-match: named binding (acknowledged)
    let _guard = m.lock();
}
