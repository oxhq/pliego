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
    pub document_decoded_pixels: u64,
    pub document_decompressed_bytes: u64,
}

pub const IMAGE_LIMITS: ImageLimits = ImageLimits {
    encoded_bytes: 64 * 1024 * 1024,
    declared_dimension: 16_384,
    decoded_pixels: 100_000_000,
    decompressed_bytes: 256 * 1024 * 1024,
    document_decoded_pixels: 100_000_000,
    document_decompressed_bytes: 256 * 1024 * 1024,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageLimit {
    EncodedBytes,
    DeclaredWidth,
    DeclaredHeight,
    DecodedPixels,
    DecompressedBytes,
    DocumentDecodedPixels,
    DocumentDecompressedBytes,
}

impl fmt::Display for ImageLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EncodedBytes => "encoded-byte",
            Self::DeclaredWidth => "declared-width",
            Self::DeclaredHeight => "declared-height",
            Self::DecodedPixels => "decoded-pixel",
            Self::DecompressedBytes => "decompressed-byte",
            Self::DocumentDecodedPixels => "document-decoded-pixel",
            Self::DocumentDecompressedBytes => "document-decompressed-byte",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImageLimitExceeded {
    pub limit: ImageLimit,
    pub configured: u64,
    pub observed: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ImageBudget {
    decoded_pixels: u64,
    decompressed_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImageCharge {
    document_pixels: u64,
    document_bytes: u64,
}

impl ImageBudget {
    pub fn check(
        &self,
        width: u64,
        height: u64,
        decompressed_bytes_per_pixel: u64,
    ) -> Result<ImageCharge, ImageLimitExceeded> {
        let (pixels, decompressed_bytes) =
            decoded_image_cost(IMAGE_LIMITS, width, height, decompressed_bytes_per_pixel)?;
        let document_pixels = observed_sum(self.decoded_pixels, pixels);
        check(
            ImageLimit::DocumentDecodedPixels,
            IMAGE_LIMITS.document_decoded_pixels,
            document_pixels,
        )?;
        let document_bytes = observed_sum(self.decompressed_bytes, decompressed_bytes);
        check(
            ImageLimit::DocumentDecompressedBytes,
            IMAGE_LIMITS.document_decompressed_bytes,
            document_bytes,
        )?;

        Ok(ImageCharge {
            document_pixels,
            document_bytes,
        })
    }

    pub fn commit(&mut self, charge: ImageCharge) {
        self.decoded_pixels = charge.document_pixels;
        self.decompressed_bytes = charge.document_bytes;
    }
}

pub(crate) fn check_encoded_bytes(encoded_bytes: usize) -> Result<(), ImageLimitExceeded> {
    check(
        ImageLimit::EncodedBytes,
        IMAGE_LIMITS.encoded_bytes,
        u64::try_from(encoded_bytes).unwrap_or(u64::MAX),
    )
}

fn decoded_image_cost(
    limits: ImageLimits,
    width: u64,
    height: u64,
    decompressed_bytes_per_pixel: u64,
) -> Result<(u64, u64), ImageLimitExceeded> {
    check(ImageLimit::DeclaredWidth, limits.declared_dimension, width)?;
    check(
        ImageLimit::DeclaredHeight,
        limits.declared_dimension,
        height,
    )?;
    let pixels = observed_product(width, height);
    check(ImageLimit::DecodedPixels, limits.decoded_pixels, pixels)?;
    let decompressed_bytes = observed_product(pixels, decompressed_bytes_per_pixel);
    check(
        ImageLimit::DecompressedBytes,
        limits.decompressed_bytes,
        decompressed_bytes,
    )?;
    Ok((pixels, decompressed_bytes))
}

fn observed_product(left: u64, right: u64) -> u64 {
    left.checked_mul(right).unwrap_or(u64::MAX)
}

fn observed_sum(left: u64, right: u64) -> u64 {
    left.checked_add(right).unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use super::{
        IMAGE_LIMITS, ImageBudget, ImageLimit, ImageLimitExceeded, ImageLimits,
        check_encoded_bytes, decoded_image_cost, observed_sum,
    };

    fn charge(
        budget: &mut ImageBudget,
        width: u64,
        height: u64,
        decompressed_bytes_per_pixel: u64,
    ) -> Result<(), ImageLimitExceeded> {
        let charge = budget.check(width, height, decompressed_bytes_per_pixel)?;
        budget.commit(charge);
        Ok(())
    }

    #[test]
    fn accepts_exact_per_image_boundaries_and_rejects_height_plus_one() {
        assert_eq!(
            check_encoded_bytes(usize::try_from(IMAGE_LIMITS.encoded_bytes).unwrap()),
            Ok(())
        );
        assert_eq!(
            decoded_image_cost(IMAGE_LIMITS, 1, IMAGE_LIMITS.declared_dimension, 4),
            Ok((IMAGE_LIMITS.declared_dimension, 65_536))
        );
        assert_eq!(
            decoded_image_cost(IMAGE_LIMITS, 1, IMAGE_LIMITS.declared_dimension + 1, 4,),
            Err(ImageLimitExceeded {
                limit: ImageLimit::DeclaredHeight,
                configured: IMAGE_LIMITS.declared_dimension,
                observed: IMAGE_LIMITS.declared_dimension + 1,
            })
        );
        assert_eq!(
            decoded_image_cost(IMAGE_LIMITS, 10_000, 10_000, 0),
            Ok((IMAGE_LIMITS.decoded_pixels, 0))
        );
        assert_eq!(
            decoded_image_cost(IMAGE_LIMITS, 8_192, 8_192, 4),
            Ok((67_108_864, IMAGE_LIMITS.decompressed_bytes))
        );
    }

    #[test]
    fn rejects_document_wide_pixel_and_decompressed_byte_overages_transactionally() {
        let mut pixel_budget = ImageBudget::default();
        charge(&mut pixel_budget, 10_000, 10_000, 0).unwrap();
        assert_eq!(
            pixel_budget.check(1, 1, 0),
            Err(ImageLimitExceeded {
                limit: ImageLimit::DocumentDecodedPixels,
                configured: IMAGE_LIMITS.document_decoded_pixels,
                observed: IMAGE_LIMITS.document_decoded_pixels + 1,
            })
        );
        assert_eq!(
            pixel_budget.decoded_pixels,
            IMAGE_LIMITS.document_decoded_pixels
        );
        assert_eq!(pixel_budget.decompressed_bytes, 0);

        let mut byte_budget = ImageBudget::default();
        charge(&mut byte_budget, 8_192, 8_192, 4).unwrap();
        assert_eq!(
            byte_budget.check(1, 1, 4),
            Err(ImageLimitExceeded {
                limit: ImageLimit::DocumentDecompressedBytes,
                configured: IMAGE_LIMITS.document_decompressed_bytes,
                observed: IMAGE_LIMITS.document_decompressed_bytes + 4,
            })
        );
        assert_eq!(byte_budget.decoded_pixels, 67_108_864);
        assert_eq!(
            byte_budget.decompressed_bytes,
            IMAGE_LIMITS.document_decompressed_bytes
        );
    }

    #[test]
    fn arithmetic_overflow_is_reported_as_the_maximum_observed_value() {
        assert_eq!(observed_sum(u64::MAX, 1), u64::MAX);
        let multiplication_limits = ImageLimits {
            encoded_bytes: u64::MAX,
            declared_dimension: u64::MAX,
            decoded_pixels: u64::MAX - 1,
            decompressed_bytes: u64::MAX,
            document_decoded_pixels: u64::MAX,
            document_decompressed_bytes: u64::MAX,
        };
        assert_eq!(
            decoded_image_cost(multiplication_limits, u64::MAX, 2, 0),
            Err(ImageLimitExceeded {
                limit: ImageLimit::DecodedPixels,
                configured: u64::MAX - 1,
                observed: u64::MAX,
            })
        );

        let addition_budget = ImageBudget {
            decoded_pixels: IMAGE_LIMITS.document_decoded_pixels - 1,
            decompressed_bytes: 0,
        };
        assert_eq!(
            addition_budget.check(2, 1, 0),
            Err(ImageLimitExceeded {
                limit: ImageLimit::DocumentDecodedPixels,
                configured: IMAGE_LIMITS.document_decoded_pixels,
                observed: IMAGE_LIMITS.document_decoded_pixels + 1,
            })
        );
    }
}
