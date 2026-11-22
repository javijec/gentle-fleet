//! Binario `fleet-ssh-bridge`, ejecutado en la máquina remota por SSH.

fn main() {
    if let Err(error) = fleet_ssh::run_bridge() {
        eprintln!("fleet-ssh-bridge: {error}");
        std::process::exit(1);
    }
}
