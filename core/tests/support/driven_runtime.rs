use std::thread::JoinHandle;

use tokio::runtime::Handle;
use tokio::sync::oneshot;

/// A current-thread runtime driven on a thread of its own until dropped, so the test thread may block on it while its tasks run.
pub struct DrivenRuntime {
    handle: Handle,
    stop_driver: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl DrivenRuntime {
    /// Starts the runtime on a thread of its own and returns once its handle is ready.
    pub fn start() -> Self {
        let (handle_sender, handle_receiver) = std::sync::mpsc::channel();
        let (stop_driver, stopped) = oneshot::channel::<()>();
        let driver = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            handle_sender.send(runtime.handle().clone()).unwrap();
            runtime.block_on(async {
                drop(stopped.await);
            });
        });
        Self {
            handle: handle_receiver.recv().unwrap(),
            stop_driver: Some(stop_driver),
            driver: Some(driver),
        }
    }

    /// The handle the test thread blocks on and spawns the runtime's tasks with.
    pub const fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for DrivenRuntime {
    fn drop(&mut self) {
        drop(self.stop_driver.take());
        if let Some(driver) = self.driver.take() {
            drop(driver.join());
        }
    }
}
