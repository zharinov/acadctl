use std::{error, fmt};

use image::{
    ColorType, GenericImageView, ImageEncoder, Rgb, RgbImage, Rgba,
    codecs::png::PngEncoder,
    imageops::{crop_imm, thumbnail},
};

const BYTES_PER_PIXEL: usize = 4;
const MAX_WIDTH: u32 = 1024;
const MAX_HEIGHT: u32 = 768;

#[derive(Clone, Copy, Debug)]
pub struct CapturedFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixel_format: PixelFormat,
    pub row_order: RowOrder,
    pub pixels: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Bgrx8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowOrder {
    TopDown,
    BottomUp,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedCrop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl NormalizedCrop {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    pub fn validate(self) -> Result<(), ScreenshotError> {
        let coordinates = [self.left, self.top, self.right, self.bottom];
        if coordinates
            .iter()
            .any(|coordinate| !coordinate.is_finite() || !(0.0..=1.0).contains(coordinate))
            || self.left >= self.right
            || self.top >= self.bottom
        {
            return Err(ScreenshotError::InvalidCrop);
        }

        Ok(())
    }

    fn pixel_bounds(self, width: u32, height: u32) -> Result<PixelBounds, ScreenshotError> {
        self.validate()?;

        let left = scale_crop_edge(self.left, width, EdgeRounding::Down);
        let top = scale_crop_edge(self.top, height, EdgeRounding::Down);
        let right = scale_crop_edge(self.right, width, EdgeRounding::Up);
        let bottom = scale_crop_edge(self.bottom, height, EdgeRounding::Up);

        Ok(PixelBounds {
            left,
            top,
            width: right - left,
            height: bottom - top,
        })
    }
}

impl PixelFormat {
    const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Bgra8 | Self::Bgrx8 => BYTES_PER_PIXEL,
        }
    }
}

#[derive(Debug)]
pub enum ScreenshotError {
    EmptyFrame,
    DimensionOverflow,
    StrideTooSmall { minimum: usize, actual: usize },
    BufferTooSmall { minimum: usize, actual: usize },
    InvalidCrop,
    PngEncoding(image::ImageError),
}

impl fmt::Display for ScreenshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => formatter.write_str("the captured frame has an empty dimension"),
            Self::DimensionOverflow => {
                formatter.write_str("the captured frame dimensions are too large")
            }
            Self::StrideTooSmall { minimum, actual } => write!(
                formatter,
                "the captured frame stride is {actual} bytes; at least {minimum} bytes are required"
            ),
            Self::BufferTooSmall { minimum, actual } => write!(
                formatter,
                "the captured frame buffer is {actual} bytes; at least {minimum} bytes are required"
            ),
            Self::InvalidCrop => formatter.write_str(
                "the normalized crop must have finite, ordered coordinates between zero and one",
            ),
            Self::PngEncoding(error) => {
                write!(formatter, "could not encode screenshot PNG: {error}")
            }
        }
    }
}

impl error::Error for ScreenshotError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::PngEncoding(error) => Some(error),
            Self::EmptyFrame
            | Self::DimensionOverflow
            | Self::StrideTooSmall { .. }
            | Self::BufferTooSmall { .. }
            | Self::InvalidCrop => None,
        }
    }
}

#[derive(Debug)]
pub struct EncodedScreenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn encode_png(
    frame: CapturedFrame<'_>,
    crop: NormalizedCrop,
) -> Result<EncodedScreenshot, ScreenshotError> {
    let image = process(frame, crop)?;
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ColorType::Rgb8.into(),
        )
        .map_err(ScreenshotError::PngEncoding)?;
    Ok(EncodedScreenshot {
        png,
        width: image.width(),
        height: image.height(),
    })
}

fn process(frame: CapturedFrame<'_>, crop: NormalizedCrop) -> Result<RgbImage, ScreenshotError> {
    validate_frame(frame)?;
    let bounds = crop.pixel_bounds(frame.width, frame.height)?;
    let (target_width, target_height) = fitted_dimensions(bounds.width, bounds.height);
    let source = FrameView(frame);
    let cropped = crop_imm(
        &source,
        bounds.left,
        bounds.top,
        bounds.width,
        bounds.height,
    );
    let resized = thumbnail(&*cropped, target_width, target_height);

    Ok(RgbImage::from_fn(target_width, target_height, |x, y| {
        let pixel = resized.get_pixel(x, y);
        Rgb([pixel[2], pixel[1], pixel[0]])
    }))
}

struct FrameView<'a>(CapturedFrame<'a>);

impl GenericImageView for FrameView<'_> {
    type Pixel = Rgba<u8>;

    fn dimensions(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }

    fn get_pixel(&self, x: u32, y: u32) -> Self::Pixel {
        let stored_y = match self.0.row_order {
            RowOrder::TopDown => y,
            RowOrder::BottomUp => self.0.height - y - 1,
        };
        let pixel_start =
            stored_y as usize * self.0.stride + x as usize * self.0.pixel_format.bytes_per_pixel();
        Rgba([
            self.0.pixels[pixel_start],
            self.0.pixels[pixel_start + 1],
            self.0.pixels[pixel_start + 2],
            255,
        ])
    }
}

fn validate_frame(frame: CapturedFrame<'_>) -> Result<(), ScreenshotError> {
    if frame.width == 0 || frame.height == 0 {
        return Err(ScreenshotError::EmptyFrame);
    }

    let row_bytes = usize::try_from(frame.width)
        .map_err(|_| ScreenshotError::DimensionOverflow)?
        .checked_mul(frame.pixel_format.bytes_per_pixel())
        .ok_or(ScreenshotError::DimensionOverflow)?;
    if frame.stride < row_bytes {
        return Err(ScreenshotError::StrideTooSmall {
            minimum: row_bytes,
            actual: frame.stride,
        });
    }

    let previous_rows = usize::try_from(frame.height - 1)
        .map_err(|_| ScreenshotError::DimensionOverflow)?
        .checked_mul(frame.stride)
        .ok_or(ScreenshotError::DimensionOverflow)?;
    let minimum = previous_rows
        .checked_add(row_bytes)
        .ok_or(ScreenshotError::DimensionOverflow)?;
    if frame.pixels.len() < minimum {
        return Err(ScreenshotError::BufferTooSmall {
            minimum,
            actual: frame.pixels.len(),
        });
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum EdgeRounding {
    Down,
    Up,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "crop coordinates are finite and bounded to [0, 1], and dimensions are u32"
)]
fn scale_crop_edge(coordinate: f64, dimension: u32, rounding: EdgeRounding) -> u32 {
    let scaled = coordinate * f64::from(dimension);
    match rounding {
        EdgeRounding::Down => scaled.floor() as u32,
        EdgeRounding::Up => scaled.ceil() as u32,
    }
}

#[derive(Clone, Copy)]
struct PixelBounds {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

fn fitted_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width <= MAX_WIDTH && height <= MAX_HEIGHT {
        return (width, height);
    }

    if u64::from(MAX_WIDTH) * u64::from(height) <= u64::from(MAX_HEIGHT) * u64::from(width) {
        let fitted_height = u64::from(height) * u64::from(MAX_WIDTH) / u64::from(width);
        (
            MAX_WIDTH,
            u32::try_from(fitted_height.max(1)).expect("fitted height is bounded by MAX_HEIGHT"),
        )
    } else {
        let fitted_width = u64::from(width) * u64::from(MAX_HEIGHT) / u64::from(height);
        (
            u32::try_from(fitted_width.max(1)).expect("fitted width is bounded by MAX_WIDTH"),
            MAX_HEIGHT,
        )
    }
}

#[cfg(test)]
mod tests {
    use image::ImageFormat;

    use super::*;

    #[test]
    fn rejects_empty_dimensions() {
        let error = process(
            CapturedFrame {
                width: 0,
                height: 1,
                stride: 0,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &[],
            },
            NormalizedCrop::FULL,
        )
        .unwrap_err();

        assert!(matches!(error, ScreenshotError::EmptyFrame));
    }

    #[test]
    fn rejects_short_stride() {
        let error = process(
            CapturedFrame {
                width: 2,
                height: 1,
                stride: 7,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &[0; 8],
            },
            NormalizedCrop::FULL,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ScreenshotError::StrideTooSmall {
                minimum: 8,
                actual: 7
            }
        ));
    }

    #[test]
    fn rejects_short_buffer_with_padded_stride() {
        let error = process(
            CapturedFrame {
                width: 1,
                height: 2,
                stride: 8,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &[0; 11],
            },
            NormalizedCrop::FULL,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ScreenshotError::BufferTooSmall {
                minimum: 12,
                actual: 11
            }
        ));
    }

    #[test]
    fn rejects_overflowing_frame_extent() {
        let error = process(
            CapturedFrame {
                width: 1,
                height: 2,
                stride: usize::MAX,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &[],
            },
            NormalizedCrop::FULL,
        )
        .unwrap_err();

        assert!(matches!(error, ScreenshotError::DimensionOverflow));
    }

    #[test]
    fn converts_bottom_up_bgra_to_top_down_rgb_and_ignores_padding() {
        let pixels = [
            255, 0, 0, 17, 255, 255, 255, 18, 0, 0, 0, 0, // bottom: blue, white
            0, 0, 255, 19, 0, 255, 0, 20, 0, 0, 0, 0, // top: red, green
        ];
        let image = process(
            CapturedFrame {
                width: 2,
                height: 2,
                stride: 12,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &pixels,
            },
            NormalizedCrop::FULL,
        )
        .unwrap();

        assert_eq!(image.get_pixel(0, 0), &Rgb([255, 0, 0]));
        assert_eq!(image.get_pixel(1, 0), &Rgb([0, 255, 0]));
        assert_eq!(image.get_pixel(0, 1), &Rgb([0, 0, 255]));
        assert_eq!(image.get_pixel(1, 1), &Rgb([255, 255, 255]));
    }

    #[test]
    fn respects_explicit_top_down_row_order() {
        let image = process(
            CapturedFrame {
                width: 1,
                height: 2,
                stride: 4,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::TopDown,
                pixels: &[0, 0, 255, 255, 255, 0, 0, 255],
            },
            NormalizedCrop::FULL,
        )
        .unwrap();

        assert_eq!(image.get_pixel(0, 0), &Rgb([255, 0, 0]));
        assert_eq!(image.get_pixel(0, 1), &Rgb([0, 0, 255]));
    }

    #[test]
    fn crop_rounds_outward_in_top_left_coordinates() {
        let mut pixels = Vec::new();
        for source_y_from_bottom in 0_u8..4 {
            for source_x in 0_u8..4 {
                pixels.extend_from_slice(&[0, source_y_from_bottom, source_x, 255]);
            }
        }
        let image = process(
            CapturedFrame {
                width: 4,
                height: 4,
                stride: 16,
                pixel_format: PixelFormat::Bgrx8,
                row_order: RowOrder::BottomUp,
                pixels: &pixels,
            },
            NormalizedCrop {
                left: 0.26,
                top: 0.26,
                right: 0.74,
                bottom: 0.74,
            },
        )
        .unwrap();

        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0), &Rgb([1, 2, 0]));
        assert_eq!(image.get_pixel(1, 1), &Rgb([2, 1, 0]));
    }

    #[test]
    fn rejects_non_finite_reversed_and_out_of_bounds_crops() {
        let pixels = [0; 16];
        let frame = CapturedFrame {
            width: 2,
            height: 2,
            stride: 8,
            pixel_format: PixelFormat::Bgra8,
            row_order: RowOrder::BottomUp,
            pixels: &pixels,
        };
        let invalid = [
            NormalizedCrop {
                left: f64::NAN,
                ..NormalizedCrop::FULL
            },
            NormalizedCrop {
                left: 0.8,
                right: 0.2,
                ..NormalizedCrop::FULL
            },
            NormalizedCrop {
                right: 1.1,
                ..NormalizedCrop::FULL
            },
        ];

        for crop in invalid {
            assert!(matches!(
                process(frame, crop),
                Err(ScreenshotError::InvalidCrop)
            ));
        }
    }

    #[test]
    fn fits_wide_and_tall_images_within_the_output_bounds() {
        assert_eq!(fitted_dimensions(2000, 1000), (1024, 512));
        assert_eq!(fitted_dimensions(1000, 2000), (384, 768));
        assert_eq!(fitted_dimensions(1200, 1200), (768, 768));
    }

    #[test]
    fn does_not_upscale_small_images() {
        assert_eq!(fitted_dimensions(640, 480), (640, 480));
        assert_eq!(fitted_dimensions(1024, 768), (1024, 768));
    }

    #[test]
    fn encoded_png_decodes_as_rgb8_with_processed_pixels() {
        let encoded = encode_png(
            CapturedFrame {
                width: 1,
                height: 1,
                stride: 4,
                pixel_format: PixelFormat::Bgra8,
                row_order: RowOrder::BottomUp,
                pixels: &[7, 11, 13, 255],
            },
            NormalizedCrop::FULL,
        )
        .unwrap();
        assert_eq!((encoded.width, encoded.height), (1, 1));
        let decoded = image::load_from_memory_with_format(&encoded.png, ImageFormat::Png).unwrap();

        assert_eq!(
            decoded.as_rgb8().unwrap().get_pixel(0, 0),
            &Rgb([13, 11, 7])
        );
    }
}
