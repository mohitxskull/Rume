//! OpenAPI export and TypeScript SDK codegen commands for `rume openapi`.

pub mod ts;

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::cli::OpenapiCommands;

const OPENAPI_START_TAG: &str = "__RUME_OPENAPI_JSON_START__";
const OPENAPI_END_TAG: &str = "__RUME_OPENAPI_JSON_END__";

pub fn parse_openapi_json(stdout: &str) -> Result<Value, String> {
    let json_str = stdout
        .find(OPENAPI_START_TAG)
        .and_then(|start| {
            let after_start = &stdout[start + OPENAPI_START_TAG.len()..];
            after_start
                .find(OPENAPI_END_TAG)
                .map(|end| after_start[..end].trim())
        })
        .ok_or_else(|| {
            format!("application did not produce OpenAPI inspection metadata.\n{stdout}")
        })?;

    serde_json::from_str(json_str)
        .map_err(|err| format!("failed to parse OpenAPI JSON: {err}\nPayload: {json_str}"))
}

pub fn run(command: OpenapiCommands) {
    match command {
        OpenapiCommands::Export { bin, out } => {
            let spec = fetch_openapi_from_binary(bin.as_deref());
            let pretty = serde_json::to_string_pretty(&spec).unwrap_or_else(|_| "{}".to_string());
            write_output(out.as_deref(), &pretty);
        }
        OpenapiCommands::Ts { bin, input, out } => {
            let spec = match input {
                Some(path) => read_openapi_file(&path),
                None => fetch_openapi_from_binary(bin.as_deref()),
            };
            let generated = ts::generate_typescript(&spec);
            write_output(out.as_deref(), &generated);
        }
    }
}

fn fetch_openapi_from_binary(bin: Option<&str>) -> Value {
    crate::commands::require_rume_project();

    let mut args = vec!["run", "--quiet"];
    if let Some(b) = bin {
        args.push("--bin");
        args.push(b);
    }
    args.extend(["--", "--rume-inspect-openapi"]);

    let output = match Command::new("cargo")
        .args(&args)
        .env("RUME_INSPECT", "openapi")
        .output()
    {
        Ok(o) => o,
        Err(err) => {
            eprintln!("error: failed to execute cargo to export OpenAPI spec: {err}");
            std::process::exit(1);
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        eprintln!("error: failed to export OpenAPI spec.\n{stderr}\n{stdout}");
        std::process::exit(output.status.code().unwrap_or(1));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_openapi_json(&stdout) {
        Ok(spec) => spec,
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

fn read_openapi_file(path: &Path) -> Value {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("error: failed to read '{}': {err}", path.display());
            std::process::exit(1);
        }
    };

    match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            eprintln!(
                "error: failed to parse OpenAPI JSON in '{}': {err}",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

fn write_output(out: Option<&Path>, content: &str) {
    match out {
        Some(path) => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
                && let Err(err) = std::fs::create_dir_all(parent)
            {
                eprintln!(
                    "error: failed to create parent directory '{}': {err}",
                    parent.display()
                );
                std::process::exit(1);
            }
            if let Err(err) = std::fs::write(path, content) {
                eprintln!("error: failed to write '{}': {err}", path.display());
                std::process::exit(1);
            }
        }
        None => println!("{content}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openapi_json_extracts_tagged_payload() {
        let stdout = "\
booting app...\n\
__RUME_OPENAPI_JSON_START__\n\
{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"Test\",\"version\":\"1.0.0\"},\"paths\":{}}\n\
__RUME_OPENAPI_JSON_END__\n\
done\n";

        let spec = parse_openapi_json(stdout).expect("tagged OpenAPI JSON should parse");
        assert_eq!(spec["openapi"], "3.1.0");
        assert_eq!(spec["info"]["title"], "Test");
        assert!(parse_openapi_json("no tags here").is_err());
    }
}
