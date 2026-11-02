//! `init` and `accept-eula`, in the test's own folders only.
use meridian_panel::bench::Bench;
use meridian_panel::init;
use std::path::{Path, PathBuf};

/// A folder of the test's own, emptied first.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meridian-panel-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn programs(dir: &Path) -> (PathBuf, PathBuf) {
    let server = dir.join("horizon-server.exe");
    let meridian = dir.join("meridian.exe");
    std::fs::write(&server, "").unwrap();
    std::fs::write(&meridian, "").unwrap();
    (server, meridian)
}

#[test]
fn init_writes_a_coherent_bench_without_accepting_the_eula() {
    let dir = scratch("init");
    let (server, meridian) = programs(&dir);
    let bench_dir = dir.join("bench");
    let out = init::init(&init::InitOptions {
        dir: bench_dir.clone(),
        horizon_server: server,
        meridian,
        resources: None,
    })
    .unwrap();

    let bench = Bench::load(&bench_dir.join("bench.toml")).unwrap();
    let names: Vec<&str> = bench.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["proxy", "lobby", "game1", "game2"]);

    // The proxy's configuration is valid and knows the three servers.
    let config =
        meridian_config::Config::load(&bench_dir.join("proxy").join("meridian.toml")).unwrap();
    assert_eq!(config.backends.len(), 3);
    assert_eq!(config.default_backend, "lobby");
    for (name, port, _) in init::SERVERS {
        assert_eq!(
            config.backend(name).unwrap().address,
            format!("127.0.0.1:{port}")
        );
        let server_dir = bench_dir.join(name);
        // The same secret on both sides, and no EULA written.
        assert_eq!(
            std::fs::read_to_string(server_dir.join("forwarding.secret")).unwrap(),
            config.forwarding_secret
        );
        assert!(!server_dir.join("eula.txt").exists());
        let toml = std::fs::read_to_string(server_dir.join("server.toml")).unwrap();
        assert!(toml.contains(&format!("port = {port}")));
        assert!(toml.contains("bind = \"127.0.0.1\""));
        assert!(toml.contains("online_mode = false"));
    }
    // The secret is in no file the panel shows, nor in what init prints.
    let bench_text = std::fs::read_to_string(bench_dir.join("bench.toml")).unwrap();
    assert!(!bench_text.contains(&config.forwarding_secret));
    assert!(!out.contains(&config.forwarding_secret));
    // It says how the user accepts the EULA, and what the bench costs.
    assert!(out.contains("https://aka.ms/MinecraftEULA"));
    assert!(out.contains("--i-accept-the-minecraft-eula"));
    assert!(out.contains("1.5 GB"));

    // A second init does not overwrite the bench.
    let (server, meridian) = programs(&dir);
    let again = init::init(&init::InitOptions {
        dir: bench_dir,
        horizon_server: server,
        meridian,
        resources: None,
    });
    assert!(again.unwrap_err().contains("already exists"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_refuses_programs_that_are_not_there() {
    let dir = scratch("init-missing");
    let e = init::init(&init::InitOptions {
        dir: dir.join("bench"),
        horizon_server: dir.join("nope.exe"),
        meridian: dir.join("nope2.exe"),
        resources: None,
    })
    .unwrap_err();
    assert!(e.contains("is not a file"));
    assert!(!dir.join("bench").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn accept_eula_needs_its_flag_and_writes_only_server_folders() {
    // Only on a bench made for this test.
    let dir = scratch("accept");
    let (server, meridian) = programs(&dir);
    let bench_dir = dir.join("bench");
    init::init(&init::InitOptions {
        dir: bench_dir.clone(),
        horizon_server: server,
        meridian,
        resources: None,
    })
    .unwrap();
    let refused = init::accept_eula(&bench_dir, false).unwrap_err();
    assert!(refused.contains("--i-accept-the-minecraft-eula"));
    assert!(!bench_dir.join("lobby").join("eula.txt").exists());

    let out = init::accept_eula(&bench_dir, true).unwrap();
    assert!(out.contains("https://aka.ms/MinecraftEULA"));
    for name in ["lobby", "game1", "game2"] {
        assert!(
            meridian_panel::bench::eula_accepted(&bench_dir.join(name)),
            "{name}"
        );
    }
    assert!(!bench_dir.join("proxy").join("eula.txt").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
