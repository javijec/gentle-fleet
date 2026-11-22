//! Ejecutor de la suite común de conformidad.
//!
//! Uso: fleet-conformance [--providers p1,p2,...] [--real] [--json]
//! El directorio del reporte JSON se configura con FLEET_CONFORMANCE_REPORT_DIR.

use std::time::Duration;

use fleet_conformance::{build_report, run_all, write_report, Options, Status, REAL_PROVIDERS};

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
    let report = build_report(results, &providers);

    if let Ok(directory) = std::env::var("FLEET_CONFORMANCE_REPORT_DIR") {
        match write_report(&report, std::path::Path::new(&directory)) {
            Ok(path) => eprintln!("conformance report: {}", path.display()),
            Err(error) => {
                eprintln!("ERROR: cannot write conformance report: {error}");
                std::process::exit(1);
            }
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        println!(
            "commit={} branch={} platform={} status={}",
            report.commit_sha, report.branch, report.platform, report.global_status
        );
        const HEADER: &str = "PROVIDER       CHECK                        STATUS       DETAIL";
        println!("{HEADER}");
        for result in &report.results {
            println!(
                "{:<14} {:<28} {:<12} {}",
                result.provider,
                result.check,
                format!("{:?}", result.status).to_uppercase(),
                result.detail
            );
        }
        for provider in &report.providers {
            println!(
                "summary {} {}: PASS={} FAIL={} UNTESTED={} UNSUPPORTED={} SKIP={}",
                provider.provider,
                provider.version,
                provider.passed,
                provider.failed,
                provider.untested,
                provider.unsupported,
                provider.skipped
            );
        }
    }
    if report.global_status != "PASS"
        || report
            .results
            .iter()
            .any(|result| result.status == Status::Fail)
    {
        std::process::exit(1);
    }
}
