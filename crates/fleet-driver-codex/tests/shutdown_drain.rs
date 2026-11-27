//! Native black-box fixture, no model or installed Codex required.
#[allow(dead_code)]
#[path = "../src/rpc.rs"]
mod rpc;

use std::io::{self, Read, Write};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("app-server") {
        let mut input = Vec::new();
        io::stdin().read_to_end(&mut input).unwrap();
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        for _ in 0..1024 {
            writeln!(
                stdout,
                "{{\"jsonrpc\":\"2.0\",\"method\":\"fixture\",\"params\":{{\"text\":\"{}\"}}}}",
                "x".repeat(256)
            )
            .unwrap();
            writeln!(stderr, "{}", "x".repeat(256)).unwrap();
        }
        stdout.flush().unwrap();
        stderr.flush().unwrap();
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let dir = std::env::current_dir().unwrap();
    let mut client = rpc::RpcClient::spawn_with_program(&dir, exe.as_os_str()).unwrap();
    client.close().expect("shutdown must drain both pipes");
    println!("PASS: Codex EOF drains stdout and stderr");
}
