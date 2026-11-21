//! Ejecutor de la suite común de conformidad.
//!
//! Uso: fleet-conformance [--providers p1,p2,...] [--real] [--json]

use std::time::Duration;

use fleet_conformance::{run_all, Options, Status, REAL_PROVIDERS};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    let real = args.iter().any(|arg| arg == "--real")
        || std::env::var("GF_E2E_REAL").as_deref() == Ok("1");
    let providers = args
        .iter()
        .position(|arg| arg == "--providers")
        .and_then(|index| args.get(index + 1))
        .map(|value| value.split(',').map(str::to_string).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            REAL_PROVIDERS
                .iter()
                .map(|value| value.to_string())
                .collect()
        });

    let options = Options {
        real,
        timeout: Duration::from_secs(120),
    };
    let results = run_all(&providers, &options);

    if json {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
    } else {
        const HEADER: &str = "PROVIDER       CHECK                        STATUS       DETAIL";
        println!("{HEADER}");
        for result in &results {
            println!(
                "{:<14} {:<28} {:<12} {}",
                result.provider,
                result.check,
                format!("{:?}", result.status).to_uppercase(),
                result.detail
            );
        }
    }
    if results.iter().any(|result| result.status == Status::Fail) {
        std::process::exit(1);
    }
}
