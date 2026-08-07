use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

static TRANSIENT_WIDTH: AtomicUsize = AtomicUsize::new(0);

fn interactive() -> bool {
    static INTERACTIVE: OnceLock<bool> = OnceLock::new();
    *INTERACTIVE.get_or_init(|| std::io::stderr().is_terminal())
}

pub fn step(message: &str) {
    finish_transient();
    eprintln!("{message}");
}

pub fn transient(message: &str) {
    if !interactive() {
        return;
    }
    let width = message.chars().count();
    let previous = TRANSIENT_WIDTH.swap(width, Ordering::Relaxed);
    let padding = previous.saturating_sub(width);
    let mut stderr = std::io::stderr().lock();
    let _ = write!(stderr, "\r{message}{:padding$}", "");
    let _ = stderr.flush();
}

pub fn finish_transient() {
    let width = TRANSIENT_WIDTH.swap(0, Ordering::Relaxed);
    if width == 0 {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    let _ = write!(stderr, "\r{:width$}\r", "");
    let _ = stderr.flush();
}

pub fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        return 100;
    }
    (done.saturating_mul(100) / total).min(100)
}

#[cfg(test)]
mod tests {
    use super::percent;

    #[test]
    fn percentages_are_bounded() {
        assert_eq!(percent(0, 200), 0);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(200, 200), 100);
        assert_eq!(percent(400, 200), 100);
        assert_eq!(percent(1, 0), 100);
    }
}
