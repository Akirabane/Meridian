//! The command line: `meridian [--check] [CONFIG]`.
use std::ffi::OsString;
use std::path::PathBuf;

/// The configuration file used when none is given.
pub const DEFAULT_CONFIG: &str = "meridian.toml";

pub const USAGE: &str = "\
Usage: meridian [OPTIONS] [CONFIG]

Runs the Meridian proxy with the configuration file CONFIG (meridian.toml by
default). See meridian.example.toml for every setting.

Options:
  --check        Read and check the configuration, then exit
  -h, --help     Show this help
  -V, --version  Show the version

Environment:
  MERIDIAN_LOG   error, warn, info (the default) or debug";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the proxy.
    Run {
        config: PathBuf,
    },
    /// Check the configuration and exit.
    Check {
        config: PathBuf,
    },
    Help,
    Version,
}

/// Reads the arguments that follow the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut check = false;
    let mut config: Option<PathBuf> = None;
    let mut only_paths = false;
    for arg in args {
        let text = arg.to_string_lossy();
        if !only_paths && text.starts_with('-') {
            match text.as_ref() {
                "--check" => check = true,
                "-h" | "--help" => return Ok(Command::Help),
                "-V" | "--version" => return Ok(Command::Version),
                "--" => only_paths = true,
                other => return Err(format!("unknown option {other}")),
            }
        } else if config.is_some() {
            return Err(format!(
                "only one configuration file can be given, not also {text}"
            ));
        } else {
            config = Some(PathBuf::from(arg));
        }
    }
    let config = config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
    Ok(if check {
        Command::Check { config }
    } else {
        Command::Run { config }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_argument_runs_with_the_default_file() {
        assert_eq!(
            parse_strs(&[]),
            Ok(Command::Run {
                config: DEFAULT_CONFIG.into()
            })
        );
    }

    #[test]
    fn a_path_is_the_configuration_file() {
        assert_eq!(
            parse_strs(&["proxy.toml"]),
            Ok(Command::Run {
                config: "proxy.toml".into()
            })
        );
    }

    #[test]
    fn check_can_come_before_or_after_the_path() {
        let expected = Ok(Command::Check {
            config: "proxy.toml".into(),
        });
        assert_eq!(parse_strs(&["--check", "proxy.toml"]), expected);
        assert_eq!(parse_strs(&["proxy.toml", "--check"]), expected);
        assert_eq!(
            parse_strs(&["--check"]),
            Ok(Command::Check {
                config: DEFAULT_CONFIG.into()
            })
        );
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(parse_strs(&["--check", "-h"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
        assert_eq!(parse_strs(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn an_unknown_option_is_refused() {
        assert_eq!(
            parse_strs(&["--chek"]),
            Err("unknown option --chek".to_owned())
        );
    }

    #[test]
    fn two_paths_are_refused() {
        assert!(parse_strs(&["a.toml", "b.toml"]).is_err());
    }

    #[test]
    fn a_path_starting_with_a_dash_follows_a_double_dash() {
        assert_eq!(
            parse_strs(&["--", "-odd.toml"]),
            Ok(Command::Run {
                config: "-odd.toml".into()
            })
        );
    }
}
