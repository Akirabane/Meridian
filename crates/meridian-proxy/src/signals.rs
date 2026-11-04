//! The signals a service manager sends (Unix only). SIGTERM, which `systemctl stop` and
//! `docker stop` send, stops the proxy as Ctrl+C does; SIGHUP reads the configuration
//! file again, as the console's `reload` does. Ctrl+C (SIGINT) stays with the `ctrlc`
//! handler set in `main`, on every system.
use crate::console;
use crate::log::{info, warning};
use crate::state::Proxy;
use signal_hook::consts::{SIGHUP, SIGTERM};
use signal_hook::iterator::Signals;
use std::sync::Arc;

/// Starts the thread that waits for SIGTERM and SIGHUP. The handlers are in place when this
/// returns, so a signal sent right after is not lost.
pub fn spawn(proxy: &Arc<Proxy>) -> std::io::Result<std::thread::JoinHandle<()>> {
    let mut signals = Signals::new([SIGTERM, SIGHUP])?;
    let proxy = Arc::clone(proxy);
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for signal in signals.forever() {
                match signal {
                    SIGTERM => {
                        info!("SIGTERM: stopping");
                        proxy.stop();
                        break;
                    }
                    SIGHUP => {
                        info!("SIGHUP: reloading {}", proxy.config_path.display());
                        match console::reload_file(&proxy) {
                            Ok(lines) => {
                                for line in lines {
                                    info!("reload: {line}");
                                }
                            }
                            Err(line) => warning!("reload: {line}"),
                        }
                    }
                    _ => {}
                }
            }
        })
}
