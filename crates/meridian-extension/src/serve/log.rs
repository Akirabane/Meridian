//! Where the SDK says what it does. It says what happened, and never what a message held: a request
//! carries a player's data, and the token is in no line at all.

/// The lines the SDK writes. Implement it to send them to a logger of your own.
pub trait Log: Send + Sync {
    /// Something that happened and is as it should be: a connection, a disconnection.
    fn info(&self, line: &str);
    /// Something that went wrong, or that was refused.
    fn warn(&self, line: &str);
}

/// The default: one line on the standard error stream, with its level.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stderr;

impl Log for Stderr {
    fn info(&self, line: &str) {
        eprintln!("INFO  {line}");
    }

    fn warn(&self, line: &str) {
        eprintln!("WARN  {line}");
    }
}
