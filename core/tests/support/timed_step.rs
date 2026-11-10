use std::time::Instant;

/// A step of a test, a wait or a send, whose description and elapsed milliseconds go to the test's stderr when it ends, whether it ended or failed.
struct TimedStep<'description> {
    description: &'description str,
    started: Instant,
}

impl Drop for TimedStep<'_> {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            "failed"
        } else {
            "ended"
        };
        eprintln!(
            "step {outcome} after {} ms: {}",
            self.started.elapsed().as_millis(),
            self.description
        );
    }
}

/// Runs the step, printing its description and elapsed milliseconds to the test's stderr when it ends or fails.
pub fn timed<Outcome>(description: &str, step: impl FnOnce() -> Outcome) -> Outcome {
    let _printed_when_it_ends = TimedStep {
        description,
        started: Instant::now(),
    };
    step()
}
