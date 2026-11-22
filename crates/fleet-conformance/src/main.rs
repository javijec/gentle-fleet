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

    let workspace_id = args
        .iter()
        .position(|arg| arg == "--workspace")
        .and_then(|index| args.get(index + 1))
        .filter(|id| !id.starts_with('-') && !id.is_empty())
        .cloned();
    if workspace_id.is_none() {
        eprintln!("ERROR: --workspace <registered-workspace-id> is required; refusing to run on a personal project");
        std::process::exit(2);
    }
    let options = Options {
        real,
        timeout: Duration::from_secs(120),
        workspace_id,
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
    let unsafe_result = results.iter().any(|result| result.status == Status::Fail)
        || providers.iter().any(|provider| {
            !results.iter().any(|r| {
                r.provider == *provider
                    && r.check == "session_lifecycle"
                    && r.status == Status::Pass
            }) || !results.iter().any(|r| {
                r.provider == *provider
                    && r.check == "two_client_replay"
                    && r.status == Status::Pass
            }) || (real
                && !results.iter().any(|r| {
                    r.provider == *provider && r.check == "real_chat" && r.status == Status::Pass
                }))
        });
    if unsafe_result {
        std::process::exit(1);
    }
}
