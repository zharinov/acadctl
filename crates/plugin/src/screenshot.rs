use std::{error, fmt};

use image::{
    ColorType, GenericImageView, ImageEncoder, Rgb, RgbImage, Rgba,
    codecs::png::PngEncoder,
    imageops::{crop_imm, thumbnail},
};

const BYTES_PER_PIXEL: usize = 4;
const DEFAULT_MAX_LONG_EDGE: u32 = 512;
const WIDE_MAX_LONG_EDGE: u32 = 1024;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelBounds {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

impl PixelBounds {
    pub fn new(left: u32, top: u32, width: u32, height: u32) -> Result<Self, ScreenshotError> {
        if width == 0 || height == 0 {
            return Err(ScreenshotError::EmptyBounds);
        }
        left.checked_add(width)
            .and_then(|_| top.checked_add(height))
            .ok_or(ScreenshotError::BoundsOverflow)?;

        Ok(Self {
            left,
            top,
            width,
            height,
        })
    }

    pub const fn left(self) -> u32 {
        self.left
    }

    pub const fn top(self) -> u32 {
        self.top
    }

    pub const fn width(self) -> u32 {
        self.width
    }

    pub const fn height(self) -> u32 {
        self.height
    }

    fn validate_within(self, frame: CapturedFrame<'_>) -> Result<(), ScreenshotError> {
        let right = self
            .left
            .checked_add(self.width)
            .ok_or(ScreenshotError::BoundsOverflow)?;
        let bottom = self
            .top
            .checked_add(self.height)
            .ok_or(ScreenshotError::BoundsOverflow)?;
        if right > frame.width || bottom > frame.height {
            return Err(ScreenshotError::BoundsOutsideFrame);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResolutionPolicy {
    #[default]
    Default,
    Wide,
}

impl ResolutionPolicy {
    const fn max_long_edge(self) -> u32 {
        match self {
            Self::Default => DEFAULT_MAX_LONG_EDGE,
            Self::Wide => WIDE_MAX_LONG_EDGE,
        }
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
    EmptyBounds,
    BoundsOverflow,
    BoundsOutsideFrame,
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
            Self::EmptyBounds => formatter.write_str("the screenshot bounds are empty"),
            Self::BoundsOverflow => formatter.write_str("the screenshot bounds overflow"),
            Self::BoundsOutsideFrame => {
                formatter.write_str("the screenshot bounds extend outside the captured frame")
            }
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
            | Self::EmptyBounds
            | Self::BoundsOverflow
            | Self::BoundsOutsideFrame => None,
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
    bounds: PixelBounds,
    resolution: ResolutionPolicy,
) -> Result<EncodedScreenshot, ScreenshotError> {
    let image = process(frame, bounds, resolution)?;
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

fn process(
    frame: CapturedFrame<'_>,
    bounds: PixelBounds,
    resolution: ResolutionPolicy,
) -> Result<RgbImage, ScreenshotError> {
    validate_frame(frame)?;
    bounds.validate_within(frame)?;
    let (target_width, target_height) =
        fitted_dimensions(bounds.width(), bounds.height(), resolution.max_long_edge());
    let source = FrameView(frame);
    let cropped = crop_imm(
        &source,
        bounds.left(),
        bounds.top(),
        bounds.width(),
        bounds.height(),
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
        let row_start = usize::try_from(stored_y)
            .expect("a validated frame row fits usize")
            .checked_mul(self.0.stride)
            .expect("validated frame row offset does not overflow");
        let pixel_offset = usize::try_from(x)
            .expect("a validated frame column fits usize")
            .checked_mul(self.0.pixel_format.bytes_per_pixel())
            .expect("validated frame pixel offset does not overflow");
        let pixel_start = row_start
            .checked_add(pixel_offset)
            .expect("validated frame pixel position does not overflow");
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

fn fitted_dimensions(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    if width <= max_long_edge && height <= max_long_edge {
        return (width, height);
    }

    if width >= height {
        let fitted_height = u64::from(height) * u64::from(max_long_edge) / u64::from(width);
        (
            max_long_edge,
            u32::try_from(fitted_height.max(1))
                .expect("fitted height is bounded by the long-edge limit"),
        )
    } else {
        let fitted_width = u64::from(width) * u64::from(max_long_edge) / u64::from(height);
        (
            u32::try_from(fitted_width.max(1))
                .expect("fitted width is bounded by the long-edge limit"),
            max_long_edge,
        )
    }
}

#[cfg(test)]
mod tests {
    use image::ImageFormat;

    use super::*;

    fn bounds(left: u32, top: u32, width: u32, height: u32) -> PixelBounds {
        PixelBounds::new(left, top, width, height).unwrap()
    }

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
            bounds(0, 0, 1, 1),
            ResolutionPolicy::Default,
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
            bounds(0, 0, 2, 1),
            ResolutionPolicy::Default,
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
            bounds(0, 0, 1, 2),
            ResolutionPolicy::Default,
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
            bounds(0, 0, 1, 2),
            ResolutionPolicy::Default,
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
            bounds(0, 0, 2, 2),
            ResolutionPolicy::Default,
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
            bounds(0, 0, 1, 2),
            ResolutionPolicy::Default,
        )
        .unwrap();

        assert_eq!(image.get_pixel(0, 0), &Rgb([255, 0, 0]));
        assert_eq!(image.get_pixel(0, 1), &Rgb([0, 0, 255]));
    }

    #[test]
    fn crops_exact_pixel_bounds_in_top_left_coordinates() {
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
            bounds(1, 1, 2, 2),
            ResolutionPolicy::Default,
        )
        .unwrap();

        assert_eq!(image.dimensions(), (2, 2));
        assert_eq!(image.get_pixel(0, 0), &Rgb([1, 2, 0]));
        assert_eq!(image.get_pixel(1, 1), &Rgb([2, 1, 0]));
    }

    #[test]
    fn rejects_empty_overflowing_and_out_of_frame_bounds() {
        let pixels = [0; 16];
        let frame = CapturedFrame {
            width: 2,
            height: 2,
            stride: 8,
            pixel_format: PixelFormat::Bgra8,
            row_order: RowOrder::BottomUp,
            pixels: &pixels,
        };
        assert!(matches!(
            PixelBounds::new(0, 0, 0, 1),
            Err(ScreenshotError::EmptyBounds)
        ));
        assert!(matches!(
            PixelBounds::new(u32::MAX, 0, 2, 1),
            Err(ScreenshotError::BoundsOverflow)
        ));
        assert!(matches!(
            process(frame, bounds(1, 1, 2, 2), ResolutionPolicy::Default),
            Err(ScreenshotError::BoundsOutsideFrame)
        ));
    }

    #[test]
    fn default_resolution_caps_square_wide_and_tall_long_edges_at_512() {
        assert_eq!(fitted_dimensions(1200, 1200, 512), (512, 512));
        assert_eq!(fitted_dimensions(2000, 1000, 512), (512, 256));
        assert_eq!(fitted_dimensions(1000, 2000, 512), (256, 512));
    }

    #[test]
    fn wide_resolution_caps_long_edge_at_1024() {
        assert_eq!(fitted_dimensions(1200, 1200, 1024), (1024, 1024));
        assert_eq!(fitted_dimensions(2000, 1000, 1024), (1024, 512));
        assert_eq!(fitted_dimensions(1000, 2000, 1024), (512, 1024));
    }

    #[test]
    fn neither_resolution_policy_upscales_small_images() {
        assert_eq!(fitted_dimensions(320, 480, 512), (320, 480));
        assert_eq!(fitted_dimensions(640, 480, 1024), (640, 480));
    }

    #[test]
    fn processing_applies_each_long_edge_cap() {
        let default_pixels = vec![0; 513 * 513 * BYTES_PER_PIXEL];
        let default_image = process(
            CapturedFrame {
                width: 513,
                height: 513,
                stride: 513 * BYTES_PER_PIXEL,
                pixel_format: PixelFormat::Bgrx8,
                row_order: RowOrder::TopDown,
                pixels: &default_pixels,
            },
            bounds(0, 0, 513, 513),
            ResolutionPolicy::Default,
        )
        .unwrap();
        assert_eq!(default_image.dimensions(), (512, 512));

        let wide_pixels = vec![0; 1025 * BYTES_PER_PIXEL];
        let wide_image = process(
            CapturedFrame {
                width: 1025,
                height: 1,
                stride: 1025 * BYTES_PER_PIXEL,
                pixel_format: PixelFormat::Bgrx8,
                row_order: RowOrder::TopDown,
                pixels: &wide_pixels,
            },
            bounds(0, 0, 1025, 1),
            ResolutionPolicy::Wide,
        )
        .unwrap();
        assert_eq!(wide_image.dimensions(), (1024, 1));
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
            bounds(0, 0, 1, 1),
            ResolutionPolicy::Default,
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
