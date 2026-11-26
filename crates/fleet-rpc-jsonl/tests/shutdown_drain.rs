//! Black-box native executable: flush output on stdin EOF. This fixture is
//! not a provider and does not require Node, shell, or model credentials.

use fleet_rpc_jsonl::JsonlRpc;
use std::io::{self, Read, Write};
use std::process::Command;

fn main() {
    if let Some(mode) = std::env::args().nth(1) {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).unwrap();
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        for _ in 0..1024 {
            writeln!(
                stdout,
                "{{\"type\":\"message_update\",\"data\":\"{}\"}}",
                "x".repeat(256)
            )
            .unwrap();
            writeln!(stderr, "{}", "x".repeat(256)).unwrap();
        }
        stdout.flush().unwrap();
        stderr.flush().unwrap();
        if mode == "fail" {
            std::process::exit(7);
        }
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let dir = std::env::current_dir().unwrap();
    for mode in ["ok", "fail"] {
        let mut rpc =
            JsonlRpc::spawn_command(&dir, Command::new(&exe), &[mode.into()], &[], false).unwrap();
        if mode == "ok" {
            rpc.close()
                .expect("EOF must permit output flush and exit zero");
        } else {
            let error = rpc.close().unwrap_err();
            assert_eq!(error.code, "StopUnconfirmed");
            assert!(error.message.contains("exit_code=7"));
        }
    }
    println!("PASS: shutdown drains pipes and rejects nonzero exit");
}
