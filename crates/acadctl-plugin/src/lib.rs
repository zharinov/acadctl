#[cxx::bridge(namespace = "acadctl")]
mod ffi {
    unsafe extern "C++" {
        include!("dev_reload.h");

        #[allow(dead_code)]
        fn schedule_dev_reload();
    }

    extern "Rust" {
        fn start_dev_watcher(path: String);

        fn stop_dev_watcher();
    }
}

#[cfg(feature = "dev-reload")]
mod dev_watcher {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use notify::{Config, Event, PollWatcher, RecursiveMode, Watcher};

    static WATCHER: Mutex<Option<PollWatcher>> = Mutex::new(None);

    pub fn start(path: String) {
        let target = PathBuf::from(path);
        let watched_target = target.clone();
        let handler = move |result: notify::Result<Event>| {
            let Ok(event) = result else { return };

            if event.paths.iter().any(|path| path == &target) {
                super::ffi::schedule_dev_reload();
            }
        };
        let config = Config::default()
            .with_poll_interval(Duration::from_millis(100))
            .with_compare_contents(true);
        let Ok(mut watcher) = PollWatcher::new(handler, config) else {
            return;
        };

        if watcher
            .watch(&watched_target, RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }

        let Ok(mut active) = WATCHER.lock() else {
            return;
        };
        *active = Some(watcher);
    }

    pub fn stop() {
        if let Ok(mut active) = WATCHER.lock() {
            *active = None;
        }
    }
}

fn start_dev_watcher(path: String) {
    #[cfg(feature = "dev-reload")]
    {
        dev_watcher::start(path);
    }

    #[cfg(not(feature = "dev-reload"))]
    {
        let _ = path;
    }
}

fn stop_dev_watcher() {
    #[cfg(feature = "dev-reload")]
    dev_watcher::stop();
}
