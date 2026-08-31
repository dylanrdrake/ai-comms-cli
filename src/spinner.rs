use colored::*;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);

/// An animated terminal spinner shown while waiting on a slow call (e.g. an
/// LLM response). Runs on its own task; call `stop()` once the call resolves
/// to end the animation and clear the line.
pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl Spinner {
    pub fn start(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();
        let message = message.to_string();

        let handle = tokio::spawn(async move {
            let mut frame = 0;
            while running_clone.load(Ordering::Relaxed) {
                print!(
                    "\r{} {}",
                    FRAMES[frame % FRAMES.len()].cyan(),
                    message.yellow()
                );
                let _ = io::stdout().flush();
                frame += 1;
                tokio::time::sleep(TICK).await;
            }
        });

        Spinner { running, handle }
    }

    /// Stops the animation and clears the spinner line.
    pub async fn stop(self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.handle.await;
        print!("\r{}\r", " ".repeat(60));
        let _ = io::stdout().flush();
    }
}
