/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::fs;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    if std::env::var("CARGO_CFG_TARGET_OS")? != "windows" {
        return Ok(());
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or("unexpected Cargo OUT_DIR")?;
    let Some(angle_out) = fs::read_dir(profile_dir.join("build"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("mozangle-"))
        })
        .map(|path| path.join("out"))
        .find(|path| path.join("libEGL.dll").is_file())
    else {
        return Ok(());
    };

    for name in ["libEGL.dll", "libGLESv2.dll"] {
        fs::copy(angle_out.join(name), profile_dir.join(name))?;
    }

    println!("cargo:rerun-if-changed={}", angle_out.display());
    Ok(())
}
