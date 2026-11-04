//! The `favicon` setting and the `motd` of a forced host.
use super::*;
use crate::favicon::{base64, check_png};

/// A PNG file as far as the proxy looks: the signature, a header chunk of `width` by
/// `height`, `padding` bytes standing for the pixels, and the closing chunk.
fn png(width: u32, height: u32, padding: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(13u32.to_be_bytes());
    bytes.extend(b"IHDR");
    bytes.extend(width.to_be_bytes());
    bytes.extend(height.to_be_bytes());
    bytes.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend(vec![7u8; padding]);
    bytes.extend([0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    bytes
}

/// A folder of this test's own, empty.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-favicon-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const BACKENDS: &str = r#"
forwarding-secret = "test-secret"
[[backends]]
name = "lobby"
address = "127.0.0.1:25566"
"#;

fn load(dir: &Path, top: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str_in(&format!("{top}{BACKENDS}"), dir)
}

#[test]
fn base64_is_the_standard_one() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foob"), "Zm9vYg==");
    assert_eq!(base64(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(&[0xFB, 0xFF, 0xBF]), "+/+/");
}

#[test]
fn a_64_by_64_png_passes() {
    assert_eq!(check_png(&png(64, 64, 100)), Ok(()));
}

#[test]
fn another_size_is_refused_and_the_error_says_which() {
    let err = check_png(&png(32, 32, 0)).unwrap_err();
    assert!(err.contains("32x32") && err.contains("64x64"), "{err}");
    let err = check_png(&png(64, 128, 0)).unwrap_err();
    assert!(err.contains("64x128"), "{err}");
}

#[test]
fn what_is_not_a_png_is_refused() {
    assert!(check_png(b"").unwrap_err().contains("not a PNG"));
    assert!(check_png(b"GIF89a....").unwrap_err().contains("not a PNG"));
    // A JPEG file.
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    assert!(check_png(&jpeg).unwrap_err().contains("not a PNG"));
    // The signature, then no header chunk.
    let mut no_header = png(64, 64, 0);
    no_header[12..16].copy_from_slice(b"IDAT");
    assert!(check_png(&no_header).unwrap_err().contains("header chunk"));
}

#[test]
fn a_file_cut_short_is_refused() {
    let mut bytes = png(64, 64, 50);
    bytes.truncate(bytes.len() - 5);
    assert!(check_png(&bytes).unwrap_err().contains("cut short"));
}

#[test]
fn a_file_too_large_for_the_status_is_refused() {
    assert_eq!(check_png(&png(64, 64, MAX_FAVICON_BYTES - 45)), Ok(()));
    let err = check_png(&png(64, 64, MAX_FAVICON_BYTES)).unwrap_err();
    assert!(err.contains("bytes") && err.contains("most"), "{err}");
}

#[test]
fn there_is_no_favicon_unless_asked_for() {
    assert_eq!(load(Path::new(""), "").unwrap().favicon, None);
}

#[test]
fn the_favicon_is_read_from_a_path_relative_to_the_configuration_file() {
    let dir = folder("relative");
    let bytes = png(64, 64, 300);
    std::fs::write(dir.join("icon.png"), &bytes).unwrap();
    let favicon = load(&dir, "favicon = \"icon.png\"\n")
        .unwrap()
        .favicon
        .unwrap();
    assert_eq!(favicon.path, dir.join("icon.png"));
    assert_eq!(
        favicon.data_uri,
        format!("data:image/png;base64,{}", base64(&bytes))
    );
    // An absolute path is taken as it is.
    let absolute = format!("favicon = {:?}\n", dir.join("icon.png").to_str().unwrap());
    assert_eq!(
        load(Path::new("nowhere"), &absolute).unwrap().favicon,
        Some(favicon)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unusable_favicon_stops_the_load_and_names_the_file() {
    let dir = folder("invalid");
    std::fs::write(dir.join("small.png"), png(16, 16, 0)).unwrap();
    std::fs::write(dir.join("text.png"), b"not a picture at all, really").unwrap();
    for (file, expected) in [
        ("small.png", "16x16"),
        ("text.png", "not a PNG"),
        ("missing.png", "cannot be read"),
    ] {
        let err = load(&dir, &format!("favicon = \"{file}\"\n")).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidFavicon { .. }), "{file}");
        let message = err.to_string();
        assert!(
            message.contains(file) && message.contains(expected),
            "{message}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `reload` loads the file again, so a changed picture is a changed configuration and
/// a bad one is refused.
#[test]
fn loading_again_reads_the_file_again() {
    let dir = folder("reload");
    let toml = dir.join("meridian.toml");
    std::fs::write(&toml, format!("favicon = \"icon.png\"\n{BACKENDS}")).unwrap();
    std::fs::write(dir.join("icon.png"), png(64, 64, 10)).unwrap();
    let first = Config::load(&toml).unwrap();
    std::fs::write(dir.join("icon.png"), png(64, 64, 20)).unwrap();
    let second = Config::load(&toml).unwrap();
    assert_ne!(first, second);
    assert_ne!(first.favicon, second.favicon);
    std::fs::write(dir.join("icon.png"), png(128, 128, 20)).unwrap();
    assert!(matches!(
        Config::load(&toml),
        Err(ConfigError::InvalidFavicon { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_debug_output_does_not_print_the_picture() {
    let dir = folder("debug");
    std::fs::write(dir.join("icon.png"), png(64, 64, 2000)).unwrap();
    let config = load(&dir, "favicon = \"icon.png\"\n").unwrap();
    let shown = format!("{config:?}");
    assert!(shown.contains("icon.png"));
    assert!(shown.len() < 4000, "{} bytes", shown.len());
    let _ = std::fs::remove_dir_all(&dir);
}

fn forced(extra: &str) -> Config {
    Config::from_toml_str(&format!(
        r#"motd = "Global"
{BACKENDS}
[[forced-host]]
host = "Play.Example.com"
backend = "lobby"
motd = "Play here"
[[forced-host]]
host = "plain.example.com"
backend = "lobby"
{extra}"#
    ))
    .unwrap()
}

#[test]
fn a_forced_host_may_have_its_own_motd() {
    let config = forced("");
    assert_eq!(config.forced_hosts[0].motd.as_deref(), Some("Play here"));
    assert_eq!(config.forced_hosts[1].motd, None);
}

/// The same host matching as a login: the case, a final dot.
#[test]
fn the_motd_follows_the_host_as_the_backend_does() {
    let config = forced("");
    for host in ["play.example.com", "PLAY.example.COM", "play.example.com."] {
        assert_eq!(config.motd_for(Some(host)), "Play here", "{host}");
        assert_eq!(config.forced_backend(host), Some("lobby"), "{host}");
    }
    for host in ["plain.example.com", "example.com", "other.net", ""] {
        assert_eq!(config.motd_for(Some(host)), "Global", "{host}");
    }
    assert_eq!(config.motd_for(None), "Global");
}

#[test]
fn a_motd_on_a_forced_host_is_not_confused_with_an_unknown_key() {
    let err = Config::from_toml_str(&format!(
        "{BACKENDS}\n[[forced-host]]\nhost = \"a.example.com\"\nbackend = \"lobby\"\nicon = \"x.png\"\n"
    ))
    .unwrap_err();
    assert!(matches!(err, ConfigError::Parse(_)), "{err}");
}
