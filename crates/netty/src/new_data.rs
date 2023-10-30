use std::{
    sync::{Condvar, Mutex},
    time::Duration,
};

pub struct NewDataAvailable {
    flag: Mutex<bool>,
    condvar: Condvar,
}

impl NewDataAvailable {
    pub fn new() -> Self {
        Self { flag: Mutex::new(false), condvar: Condvar::new() }
    }

    pub fn notify(&self) {
        let mut guard = self.flag.lock().unwrap();
        *guard = true;
        self.condvar.notify_one();
    }

    pub fn wait_timeout(&self, duration: Duration) -> bool {
        let guard = self.flag.lock().unwrap();
        let (mut guard, _) =
            self.condvar.wait_timeout_while(guard, duration, |new_data| !*new_data).unwrap();
        let new_data = *guard;
        *guard = false;
        new_data
    }
}
