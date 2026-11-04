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

/// Stand-ins for the three programs, the sidecar beside `meridian` as a release
/// puts it.
fn programs(dir: &Path) -> (PathBuf, PathBuf) {
    let server = dir.join("horizon-server.exe");
    let meridian = dir.join("meridian.exe");
    let sidecar = dir.join(format!("meridian-sidecar{}", std::env::consts::EXE_SUFFIX));
    for program in [&server, &meridian, &sidecar] {
        std::fs::write(program, "").unwrap();
    }
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
        sidecar: None,
    })
    .unwrap();

    let bench = Bench::load(&bench_dir.join("bench.toml")).unwrap();
    let names: Vec<&str> = bench.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "proxy",
            "lobby",
            "game1",
            "game2",
            "lobby-sidecar",
            "game1-sidecar",
            "game2-sidecar"
        ]
    );

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
        // The switches need the server's transfer module, which is off by default.
        assert!(toml.contains("[transfer]\nenabled = true"));
    }
    // Each server has its sidecar, with a secret of its own that the proxy and
    // the sidecar read from the same file.
    let mut secrets = Vec::new();
    for (name, port) in init::SIDECARS {
        let sidecar = config.sidecars.iter().find(|s| s.backend == name).unwrap();
        assert_eq!(sidecar.address, format!("127.0.0.1:{port}"));
        let secret = sidecar.read_secret().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(secret.iter().all(u8::is_ascii_hexdigit));
        assert_eq!(
            std::fs::canonicalize(&sidecar.secret_file).unwrap(),
            std::fs::canonicalize(bench_dir.join("secrets").join(format!("{name}.key"))).unwrap()
        );
        let process = bench.process(&format!("{name}-sidecar")).unwrap();
        assert_eq!(process.kind, meridian_panel::bench::Kind::Sidecar);
        assert_eq!(process.port, port);
        assert_eq!(process.dir, bench_dir.join(name));
        assert_eq!(
            process.args,
            [
                "--listen".to_owned(),
                format!("127.0.0.1:{port}"),
                "--endpoint-file".to_owned(),
                init::ENDPOINT_FILE.to_owned(),
                "--secret-file".to_owned(),
                format!("../secrets/{name}.key"),
            ]
        );
        // The sidecar, run in the server's folder, finds the same file.
        assert_eq!(
            std::fs::canonicalize(process.dir.join(&process.args[5])).unwrap(),
            std::fs::canonicalize(&sidecar.secret_file).unwrap()
        );
        assert_eq!(process.stop_command, None);
        secrets.push(String::from_utf8(secret).unwrap());
    }
    secrets.sort();
    secrets.dedup();
    assert_eq!(secrets.len(), 3, "each sidecar has a secret of its own");

    // No secret is in a file the panel shows, nor in what init prints.
    let bench_text = std::fs::read_to_string(bench_dir.join("bench.toml")).unwrap();
    for secret in secrets.iter().chain([&config.forwarding_secret]) {
        assert!(!bench_text.contains(secret.as_str()));
        assert!(!out.contains(secret.as_str()));
    }
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
        sidecar: None,
    });
    assert!(again.unwrap_err().contains("already exists"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn init_needs_the_sidecar_program_beside_meridian_or_named() {
    let dir = scratch("init-sidecar");
    let (server, meridian) = programs(&dir);
    std::fs::remove_file(dir.join(format!("meridian-sidecar{}", std::env::consts::EXE_SUFFIX)))
        .unwrap();
    let options = |sidecar| init::InitOptions {
        dir: dir.join("bench"),
        horizon_server: server.clone(),
        meridian: meridian.clone(),
        resources: None,
        sidecar,
    };
    let e = init::init(&options(None)).unwrap_err();
    assert!(e.starts_with("meridian-sidecar:"), "{e}");
    assert!(!dir.join("bench").exists());
    let elsewhere = dir.join("tools").join("sidecar.exe");
    std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
    std::fs::write(&elsewhere, "").unwrap();
    init::init(&options(Some(elsewhere.clone()))).unwrap();
    let bench = Bench::load(&dir.join("bench").join("bench.toml")).unwrap();
    assert_eq!(bench.process("lobby-sidecar").unwrap().program, elsewhere);
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
        sidecar: None,
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
        sidecar: None,
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
