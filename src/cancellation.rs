//! Interrupt inference immediately when its configuration is replaced.
use anyhow::Result;
use std::{
    process::Child,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default)]
pub(crate) struct Cancellation(Arc<State>);

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    children: Mutex<Vec<Weak<Mutex<Child>>>>,
}

#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Settings changed; restarting with the current configuration")
    }
}
impl std::error::Error for Cancelled {}

impl Cancellation {
    pub fn check(&self) -> Result<()> {
        if self.0.cancelled.load(Ordering::Acquire) {
            return Err(Cancelled.into());
        }
        Ok(())
    }
    pub fn track(&self, child: &Arc<Mutex<Child>>) -> Result<()> {
        let mut children = self.0.children.lock().unwrap();
        children.retain(|child| child.strong_count() > 0);
        children.push(Arc::downgrade(child));
        self.check()
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        for child in self
            .0
            .children
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
        {
            // A locked child is being polled or reaped. Startup checks the token
            // before polling again; a worker waiting on HTTP holds no child lock.
            if let Ok(mut child) = child.try_lock() {
                let _ = child.kill();
            }
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct WorkControl(Arc<Mutex<Cancellation>>);
impl WorkControl {
    pub fn token(&self) -> Cancellation {
        self.0.lock().unwrap().clone()
    }
    pub fn interrupt(&self) {
        let mut current = self.0.lock().unwrap();
        current.cancel();
        *current = Cancellation::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacing_settings_terminates_the_old_process_and_allows_new_work() -> Result<()> {
        let control = WorkControl::default();
        let old = control.token();
        let child = Arc::new(Mutex::new(
            std::process::Command::new("sleep").arg("60").spawn()?,
        ));
        old.track(&child)?;
        control.interrupt();
        assert!(old.check().unwrap_err().is::<Cancelled>());
        assert!(!child.lock().unwrap().wait()?.success());
        control.token().check()?;
        Ok(())
    }
}
