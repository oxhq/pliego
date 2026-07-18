/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(not(any(target_os = "android", target_env = "ohos")))]
use std::time::{SystemTime, UNIX_EPOCH};

use session::LocalDocument;

mod session;

const SERVO_BASE_SHA: &str = "313b6d5ecc113b08010ce434140db3ca5abcc71c";

#[derive(Debug, PartialEq)]
enum Command {
    Help,
    Version,
    Render(PathBuf),
}

fn parse_args(args: Vec<OsString>) -> Result<Command, String> {
    match args.as_slice() {
        [] => Ok(Command::Help),
        [flag] if flag == "-h" || flag == "--help" => Ok(Command::Help),
        [flag] if flag == "-V" || flag == "--version" || flag == "--verbose-version" => {
            Ok(Command::Version)
        },
        [input] if !input.to_string_lossy().starts_with('-') => {
            Ok(Command::Render(PathBuf::from(input)))
        },
        _ => Err("usage: pliego <document.html>".into()),
    }
}

fn main() {
    let command = parse_args(std::env::args_os().skip(1).collect()).unwrap_or_else(|error| {
        eprintln!("pliego: {error}");
        std::process::exit(2)
    });

    match command {
        Command::Help => print_help(),
        Command::Version => print_version(),
        Command::Render(input) => render(input),
    }
}

fn print_help() {
    println!(
        "Pliego — native document rendering on Servo\n\nUsage:\n  pliego <document.html>\n  pliego --version"
    );
}

fn print_version() {
    println!(
        "pliego {}\n{}\nServo base {}",
        env!("CARGO_PKG_VERSION"),
        servoshell::VERSION,
        SERVO_BASE_SHA
    );
}

#[cfg(not(any(target_os = "android", target_env = "ohos")))]
fn render(input: PathBuf) {
    let document = LocalDocument::resolve(".", &input).unwrap_or_else(|error| {
        eprintln!("pliego: {error}");
        std::process::exit(2)
    });
    let input_url = url::Url::from_file_path(document.path()).unwrap_or_else(|_| {
        eprintln!("pliego: cannot convert document path to a file URL");
        std::process::exit(2)
    });
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let proof =
        std::env::temp_dir().join(format!("pliego-render-{}-{unique}.png", std::process::id()));

    servoshell::run(&[
        "--headless".into(),
        "--exit".into(),
        "--output".into(),
        proof.to_string_lossy().into_owned(),
        input_url.to_string(),
    ]);

    let rendered_bytes = std::fs::metadata(&proof)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if rendered_bytes == 0 {
        eprintln!("pliego: Servo did not produce a rendered image");
        std::process::exit(1);
    }
    if let Err(error) = std::fs::remove_file(&proof) {
        eprintln!(
            "pliego: warning: could not remove {}: {error}",
            proof.display()
        );
    }

    println!(
        "{}",
        serde_json::json!({
            "engine": "pliego",
            "document_root": document.root().to_string_lossy(),
            "input": document.path().to_string_lossy(),
            "servo_base_sha": SERVO_BASE_SHA,
            "servo_build": servoshell::VERSION,
            "rendered_bytes": rendered_bytes,
            "status": "rendered"
        })
    );
}

#[cfg(any(target_os = "android", target_env = "ohos"))]
fn render(_input: PathBuf) {
    eprintln!("pliego: the command-line renderer is only available on desktop targets");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::{Command, parse_args};
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn accepts_only_help_version_or_one_document() {
        assert_eq!(parse_args(vec![]).unwrap(), Command::Help);
        assert_eq!(
            parse_args(vec![OsString::from("--version")]).unwrap(),
            Command::Version
        );
        assert_eq!(
            parse_args(vec![OsString::from("invoice.html")]).unwrap(),
            Command::Render(PathBuf::from("invoice.html"))
        );
        assert!(
            parse_args(vec![
                OsString::from("invoice.html"),
                OsString::from("extra")
            ])
            .is_err()
        );
    }
}
