//! Telling the proxy about a server before the panel stops it, and after.
//!
//! A server that is stopped or restarted takes its players with it, and the proxy
//! keeps offering it to new ones until it notices. With `[drain-before-stop]` on, the
//! panel types `drain <server>` (with `--move` if asked: the players are sent to
//! another server of their role first) on the console of the proxy, waits the grace
//! the bench file gives, and only then stops the server. When the server answers
//! again, the panel types `undrain <server>`, but only if the drain was its own: a
//! server the operator had drained stays drained.
//!
//! A server that dies by itself cannot be announced, but the proxy is told at once
//! so that it sends no new player to it until it is back.
//!
//! The panel talks to the proxy through its console, the same one the operator
//! types in, and reads its answer the way the page does.
use super::*;

/// How long the answer of a plain `drain` is waited for.
const DRAIN_WAIT: Duration = Duration::from_secs(10);
/// How long the answer of `drain --move` is waited for: the proxy answers when the
/// last player has moved, half a second after the one before.
const MOVE_WAIT: Duration = Duration::from_secs(60);
/// How long the answer of `undrain` is waited for.
const UNDRAIN_WAIT: Duration = Duration::from_secs(10);
/// Lines of the proxy's answer copied into the log of the server.
const ANSWER_LINES: usize = 12;

impl Supervisor {
    pub(super) fn panel_drains(&self) -> MutexGuard<'_, HashSet<String>> {
        self.drained.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The proxy to tell, if it is running.
    fn proxy_up(&self) -> Option<&Arc<Process>> {
        let name = self.proxy.as_deref()?;
        let proxy = self.process(name).ok()?;
        (proxy.lock().state == State::Online).then_some(proxy)
    }

    /// Whether the proxy is to be told before `process` is stopped: the setting is
    /// on, it is a server that answers, and there is a proxy running to tell.
    pub(super) fn needs_notice(&self, process: &Process) -> bool {
        self.notice.enabled
            && process.spec.kind == Kind::Server
            && process.lock().state == State::Online
            && self.proxy_up().is_some()
    }

    /// Tells the proxy that `process` is about to stop and waits the grace. Returns
    /// whether the server is still to be stopped: `false` when it ended, or was
    /// stopped by someone else, in the meantime. Without a notice to give it returns
    /// `true` at once.
    pub(super) fn give_notice(&self, process: &Arc<Process>) -> bool {
        if !self.needs_notice(process) {
            return true;
        }
        let run = {
            let mut inner = process.lock();
            if inner.state != State::Online {
                return true;
            }
            inner.state = State::Draining;
            inner.running.as_ref().map(|r| r.run)
        };
        let Some(run) = run else { return false };
        let backend = process.spec.backend_name();
        let line = if self.notice.move_players {
            format!("drain {backend} --move")
        } else {
            format!("drain {backend}")
        };
        self.drain(process, &line, backend);
        let until = Instant::now() + self.notice.grace;
        loop {
            if !draining(process, run) {
                return false;
            }
            if Instant::now() >= until {
                return true;
            }
            thread::sleep(POLL_ANSWER.min(self.notice.grace));
        }
    }

    /// Types `line`, a drain of `backend`, on the proxy's console and notes the drain
    /// as the panel's if it is the panel that made it.
    fn drain(&self, process: &Process, line: &str, backend: &str) {
        let Some(proxy) = self.proxy_up() else {
            process.log().push(
                "[panel] the proxy is not running: it was not told that this server stops"
                    .to_owned(),
            );
            return;
        };
        let wait = if line.ends_with("--move") {
            MOVE_WAIT
        } else {
            DRAIN_WAIT
        };
        // The proxy's own words: "<server> takes no new player" when it drained the
        // server just now, "<server> is already drained" when the operator had. The
        // drain is noted before the answer is logged, so whoever reads the log of the
        // server knows it is noted.
        let ours = format!("{backend} takes no new player");
        let _ = self.talk(process, &proxy.spec.name, line, wait, |answer| {
            if answer.iter().any(|l| l.starts_with(&ours)) {
                self.panel_drains().insert(backend.to_owned());
            }
        });
    }

    /// Types `line` on the console of `proxy` and returns its answer, copied into the
    /// log of `process` so that whoever reads that log sees what the proxy was told.
    fn talk(
        &self,
        process: &Process,
        proxy: &str,
        line: &str,
        wait: Duration,
        on_answer: impl FnOnce(&[String]),
    ) -> Option<Vec<String>> {
        let _one_at_a_time = self.talking.lock().unwrap_or_else(|e| e.into_inner());
        match self.ask(proxy, line, wait) {
            Ok(answer) => {
                on_answer(&answer);
                let mut log = process.log();
                log.push(format!("[panel] told the proxy \"{line}\""));
                for l in answer.iter().take(ANSWER_LINES) {
                    log.push(format!("[panel] proxy: {l}"));
                }
                if answer.len() > ANSWER_LINES {
                    log.push(format!(
                        "[panel] proxy: and {} more lines",
                        answer.len() - ANSWER_LINES
                    ));
                }
                Some(answer)
            }
            Err(e) => {
                process
                    .log()
                    .push(format!("[panel] could not tell the proxy \"{line}\": {e}"));
                None
            }
        }
    }

    /// A server ended by itself: players cannot be moved off it any more, but the
    /// proxy is told so that it sends no new one to it before it is back.
    pub(super) fn after_crash(self: &Arc<Self>, process: &Arc<Process>) {
        if !self.notice.enabled || process.spec.kind != Kind::Server {
            return;
        }
        let supervisor = Arc::clone(self);
        let process = Arc::clone(process);
        let _ = thread::Builder::new()
            .name(format!("panel-crash-{}", process.spec.name))
            .spawn(move || {
                let backend = process.spec.backend_name();
                supervisor.drain(&process, &format!("drain {backend}"), backend);
            });
    }

    /// A server answers: lifts the drain the panel put on it, and no other.
    pub(super) fn welcome_back(self: &Arc<Self>, process: &Arc<Process>) {
        if !self.notice.enabled || process.spec.kind != Kind::Server {
            return;
        }
        let backend = process.spec.backend_name().to_owned();
        if !self.panel_drains().remove(&backend) {
            return;
        }
        let supervisor = Arc::clone(self);
        let process = Arc::clone(process);
        let _ = thread::Builder::new()
            .name(format!("panel-back-{}", process.spec.name))
            .spawn(move || {
                let Some(proxy) = supervisor.proxy_up() else {
                    process.log().push(
                        "[panel] the proxy is not running: the drain it had is gone with it"
                            .to_owned(),
                    );
                    return;
                };
                let line = format!("undrain {backend}");
                let _ = supervisor.talk(&process, &proxy.spec.name, &line, UNDRAIN_WAIT, |_| {});
            });
    }
}

/// Whether the run `run` of `process` is still the one that was told to stop soon.
fn draining(process: &Process, run: u64) -> bool {
    let inner = process.lock();
    inner.state == State::Draining && inner.running.as_ref().map(|r| r.run) == Some(run)
}
