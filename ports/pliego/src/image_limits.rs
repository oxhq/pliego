/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub encoded_bytes: u64,
    pub declared_dimension: u64,
    pub decoded_pixels: u64,
    pub decompressed_bytes: u64,
}

pub const IMAGE_LIMITS: ImageLimits = ImageLimits {
    encoded_bytes: 64 * 1024 * 1024,
    declared_dimension: 16_384,
    decoded_pixels: 100_000_000,
    decompressed_bytes: 256 * 1024 * 1024,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageLimit {
    EncodedBytes,
    DeclaredWidth,
    DeclaredHeight,
    DecodedPixels,
    DecompressedBytes,
}

impl fmt::Display for ImageLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EncodedBytes => "encoded-byte",
            Self::DeclaredWidth => "declared-width",
            Self::DeclaredHeight => "declared-height",
            Self::DecodedPixels => "decoded-pixel",
            Self::DecompressedBytes => "decompressed-byte",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImageLimitExceeded {
    pub limit: ImageLimit,
    pub configured: u64,
    pub observed: u64,
}

pub(crate) fn check_encoded_bytes(encoded_bytes: usize) -> Result<(), ImageLimitExceeded> {
    check(
        ImageLimit::EncodedBytes,
        IMAGE_LIMITS.encoded_bytes,
        u64::try_from(encoded_bytes).unwrap_or(u64::MAX),
    )
}

pub(crate) fn check_decoded_image(
    width: u64,
    height: u64,
    decompressed_bytes_per_pixel: u64,
) -> Result<(), ImageLimitExceeded> {
    check(
        ImageLimit::DeclaredWidth,
        IMAGE_LIMITS.declared_dimension,
        width,
    )?;
    check(
        ImageLimit::DeclaredHeight,
        IMAGE_LIMITS.declared_dimension,
        height,
    )?;
    let pixels = width.checked_mul(height).unwrap_or(u64::MAX);
    check(
        ImageLimit::DecodedPixels,
        IMAGE_LIMITS.decoded_pixels,
        pixels,
    )?;
    check(
        ImageLimit::DecompressedBytes,
        IMAGE_LIMITS.decompressed_bytes,
        pixels
            .checked_mul(decompressed_bytes_per_pixel)
            .unwrap_or(u64::MAX),
    )
}

fn check(limit: ImageLimit, configured: u64, observed: u64) -> Result<(), ImageLimitExceeded> {
    if observed <= configured {
        Ok(())
    } else {
        Err(ImageLimitExceeded {
            limit,
            configured,
            observed,
        })
    }
}
