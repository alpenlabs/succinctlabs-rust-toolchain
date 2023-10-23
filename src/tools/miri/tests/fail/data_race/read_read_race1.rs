//@compile-flags: -Zmiri-preemption-rate=0.0
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;

// Make sure races between atomic and non-atomic reads are detected.
// This seems harmless but C++ does not allow them, so we can't allow them for now either.
// This test coverse the case where the non-atomic access come first.
fn main() {
    let a = AtomicU16::new(0);

    thread::scope(|s| {
        s.spawn(|| {
            let ptr = &a as *const AtomicU16 as *mut u16;
            unsafe { ptr.read() };
        });
        s.spawn(|| {
            thread::yield_now();
            a.load(Ordering::SeqCst);
            //~^ ERROR: Data race detected between (1) Read on thread `<unnamed>` and (2) Atomic Load on thread `<unnamed>`
        });
    });
}
