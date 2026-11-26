// Deliberately silent owned process for the W1-D startup watchdog.
// It has no descendants, network, credentials or filesystem effects.
fn main() {
    std::thread::sleep(std::time::Duration::from_secs(120));
}
