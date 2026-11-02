//! The sidecar's command line: where it listens, where the backend's endpoint
//! file is, and where the shared secret comes from. The secret is never an
//! argument, since anyone on the machine can read a process's arguments.
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: meridian-sidecar --endpoint-file PATH [OPTIONS]

Runs next to a Horizon server and serves the proxy's player requests, passing
each one to the server's local control endpoint.

Options:
  --endpoint-file PATH   the endpoint file the server writes (--control-socket)
  --listen ADDR          where the proxy connects (default 127.0.0.1:25580)
  --secret-file PATH     the secret shared with the proxy (or MERIDIAN_SIDECAR_SECRET)
  --endpoint-timeout-ms N  how long the server has to answer (default 5000)
  --allow-remote         listen on an address other machines can reach
  --insecure-remote      with --allow-remote, accept it without TLS: player
                         records then cross the network in clear
  -h, --help             show this help
  -V, --version          show the version";

/// The shortest secret accepted, in bytes.
pub const MIN_SECRET: usize = 16;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Run(Options),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub listen: SocketAddr,
    pub endpoint_file: PathBuf,
    pub secret_file: Option<PathBuf>,
    pub endpoint_timeout: Duration,
    pub allow_remote: bool,
    pub insecure_remote: bool,
}

/// Reads the arguments after the program's name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut listen: SocketAddr = "127.0.0.1:25580".parse().expect("a valid default");
    let mut endpoint_file = None;
    let mut secret_file = None;
    let mut endpoint_timeout = Duration::from_millis(5_000);
    let (mut allow_remote, mut insecure_remote) = (false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|a| format!("{a:?} is not text"))?;
        let mut value = |name: &str| {
            args.next()
                .and_then(|v| v.into_string().ok())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--listen" => {
                let text = value("--listen")?;
                listen = text
                    .parse()
                    .map_err(|_| format!("{text:?} is not an address like 127.0.0.1:25580"))?;
            }
            "--endpoint-file" => endpoint_file = Some(PathBuf::from(value("--endpoint-file")?)),
            "--secret-file" => secret_file = Some(PathBuf::from(value("--secret-file")?)),
            "--endpoint-timeout-ms" => {
                let text = value("--endpoint-timeout-ms")?;
                let ms: u64 =
                    text.parse().ok().filter(|ms| *ms > 0).ok_or_else(|| {
                        format!("{text:?} is not a number of milliseconds above 0")
                    })?;
                endpoint_timeout = Duration::from_millis(ms);
            }
            "--allow-remote" => allow_remote = true,
            "--insecure-remote" => insecure_remote = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    let endpoint_file = endpoint_file.ok_or("--endpoint-file is required")?;
    Ok(Command::Run(Options {
        listen,
        endpoint_file,
        secret_file,
        endpoint_timeout,
        allow_remote,
        insecure_remote,
    }))
}

/// Refuses an address other machines can reach unless both flags say so: the
/// exchange is signed but not encrypted, so a player's record would cross the
/// network in clear.
pub fn check_listen(options: &Options) -> Result<(), String> {
    if options.listen.ip().is_loopback() {
        return Ok(());
    }
    if !options.allow_remote {
        return Err(format!(
            "{} is not a local address: the sidecar listens on 127.0.0.1 unless \
             --allow-remote is given",
            options.listen
        ));
    }
    if !options.insecure_remote {
        return Err(format!(
            "{} is reachable from other machines, and the sidecar has no TLS yet: player \
             records would cross the network in clear. Give --insecure-remote as well to \
             accept that",
            options.listen
        ));
    }
    Ok(())
}

/// The shared secret, from the file if one is given, else from the environment
/// variable's value `env`. A final line break in the file is not part of it.
pub fn secret(file_text: Option<String>, env: Option<String>) -> Result<Vec<u8>, String> {
    let secret = match (file_text, env) {
        (Some(text), _) => text.trim_end_matches(['\r', '\n']).to_owned(),
        (None, Some(value)) => value,
        (None, None) => {
            return Err("no secret: give --secret-file or set MERIDIAN_SIDECAR_SECRET".into());
        }
    };
    if secret.len() < MIN_SECRET {
        return Err(format!(
            "the secret is {} bytes; use at least {MIN_SECRET}",
            secret.len()
        ));
    }
    Ok(secret.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Options, String> {
        match parse(args.iter().map(OsString::from))? {
            Command::Run(options) => Ok(options),
            other => Err(format!("{other:?}")),
        }
    }

    #[test]
    fn the_defaults_listen_locally() {
        let options = run(&["--endpoint-file", "engine.json"]).unwrap();
        assert_eq!(options.listen, "127.0.0.1:25580".parse().unwrap());
        assert_eq!(options.endpoint_timeout, Duration::from_secs(5));
        assert!(check_listen(&options).is_ok());
        assert!(
            run(&[])
                .unwrap_err()
                .contains("--endpoint-file is required")
        );
        assert!(
            run(&["--endpoint-file"])
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(run(&["--endpoint-file", "e", "--port", "1"]).is_err());
        assert_eq!(parse([OsString::from("--help")]), Ok(Command::Help));
    }

    #[test]
    fn a_remote_address_needs_both_flags() {
        let options = |flags: &[&str]| {
            let mut args = vec!["--endpoint-file", "e", "--listen", "0.0.0.0:25580"];
            args.extend_from_slice(flags);
            run(&args).unwrap()
        };
        let refused = check_listen(&options(&[])).unwrap_err();
        assert!(refused.contains("--allow-remote"), "{refused}");
        let refused = check_listen(&options(&["--allow-remote"])).unwrap_err();
        assert!(refused.contains("no TLS yet"), "{refused}");
        assert!(check_listen(&options(&["--allow-remote", "--insecure-remote"])).is_ok());
        // --insecure-remote alone does not open anything.
        assert!(check_listen(&options(&["--insecure-remote"])).is_err());
        let local = run(&["--endpoint-file", "e", "--listen", "[::1]:25580"]).unwrap();
        assert!(check_listen(&local).is_ok());
    }

    #[test]
    fn the_secret_comes_from_the_file_or_the_environment() {
        let long = "a-secret-of-enough-length";
        assert_eq!(
            secret(
                Some(format!("{long}\r\n")),
                Some("ignored-when-a-file".into())
            ),
            Ok(long.as_bytes().to_vec())
        );
        assert_eq!(
            secret(None, Some(long.into())),
            Ok(long.as_bytes().to_vec())
        );
        assert!(secret(None, None).unwrap_err().contains("no secret"));
        assert!(
            secret(Some("short".into()), None)
                .unwrap_err()
                .contains("at least 16")
        );
    }
}
