//! Native argv fixture. The copied executable is a Rust test helper, not
//! a provider or Node emulator. No credentials, external tools or shell.

use fleet_agent::process::provider_command;
use std::ffi::OsString;
use std::path::Path;

const ARGS: &[&str] = &[
    "espacio ñ",
    "a&b|c^d",
    "%PATH%!VAR!",
    "quote\"x",
    "tail\\",
    "",
];

fn assert_child_args() {
    let mut args = std::env::args_os().skip(1);
    if let Some(prefix) = std::env::var_os("FLEET_TEST_ARG_PREFIX") {
        assert_eq!(args.next(), Some(prefix));
    }
    assert_eq!(
        args.collect::<Vec<_>>(),
        ARGS.iter().copied().map(OsString::from).collect::<Vec<_>>()
    );
}

fn run_fixture(dir: &Path) {
    #[cfg(windows)]
    let name = "provider-fixture.exe";
    #[cfg(not(windows))]
    let name = "provider-fixture";
    let exe = dir.join(name);
    std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();

    #[cfg(windows)]
    let mut command = {
        std::env::set_var("FLEET_TEST_PROVIDER_BIN", &exe);
        provider_command("provider-fixture", "FLEET_TEST_PROVIDER_BIN", None).unwrap()
    };
    #[cfg(not(windows))]
    let mut command =
        provider_command(exe.to_str().unwrap(), "FLEET_TEST_PROVIDER_BIN", None).unwrap();

    assert!(command
        .args(ARGS)
        .env("FLEET_TEST_ARG_CHILD", "1")
        .env_remove("FLEET_TEST_ARG_PREFIX")
        .current_dir(dir)
        .status()
        .unwrap()
        .success());

    #[cfg(windows)]
    {
        let node = dir.join("node.exe");
        let script = dir.join("entry ñ & safe.js");
        let shim = dir.join("provider-fixture.cmd");
        std::fs::copy(&exe, &node).unwrap();
        std::fs::write(&script, "// fixture").unwrap();
        std::fs::write(&shim, "@exit /b 99").unwrap();

        std::env::set_var("FLEET_TEST_PROVIDER_BIN", &shim);
        assert!(provider_command("provider-fixture", "FLEET_TEST_PROVIDER_BIN", None).is_err());

        std::env::set_var("FLEET_TEST_PROVIDER_JS", &script);
        assert!(provider_command(
            "provider-fixture",
            "FLEET_TEST_PROVIDER_BIN",
            Some("FLEET_TEST_PROVIDER_JS"),
        )
        .is_err());
        std::env::remove_var("FLEET_TEST_PROVIDER_BIN");
        std::env::set_var("FLEET_NODE_BIN", &node);

        let mut command = provider_command(
            "provider-fixture",
            "FLEET_TEST_PROVIDER_BIN",
            Some("FLEET_TEST_PROVIDER_JS"),
        )
        .unwrap();
        assert!(command
            .args(ARGS)
            .env("FLEET_TEST_ARG_CHILD", "1")
            .env("FLEET_TEST_ARG_PREFIX", &script)
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
    }
}

fn main() {
    if std::env::var_os("FLEET_TEST_ARG_CHILD").is_some() {
        assert_child_args();
        return;
    }
    let dir = std::env::temp_dir().join(format!("fleet argv ñ & {}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    run_fixture(&dir);
    std::fs::remove_dir_all(dir).unwrap();
    println!("PASS: native provider argv fixture");
}
