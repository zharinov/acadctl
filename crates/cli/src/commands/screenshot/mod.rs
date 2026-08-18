mod output;

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use acadctl_rpc::{ScreenshotCrop, ScreenshotRequest};
use serde::Serialize;
use time::OffsetDateTime;

use super::{RequestOperation, fail, request_error_message, target::Target};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Crop {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParseCropError;

impl fmt::Display for ParseCropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected four ordered comma-separated edges between zero and one")
    }
}

impl std::error::Error for ParseCropError {}

impl FromStr for Crop {
    type Err = ParseCropError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut values = value.split(',').map(str::parse::<f64>);
        let crop = Self {
            left: values
                .next()
                .ok_or(ParseCropError)?
                .map_err(|_| ParseCropError)?,
            top: values
                .next()
                .ok_or(ParseCropError)?
                .map_err(|_| ParseCropError)?,
            right: values
                .next()
                .ok_or(ParseCropError)?
                .map_err(|_| ParseCropError)?,
            bottom: values
                .next()
                .ok_or(ParseCropError)?
                .map_err(|_| ParseCropError)?,
        };

        if values.next().is_some()
            || [crop.left, crop.top, crop.right, crop.bottom]
                .iter()
                .any(|edge| !edge.is_finite() || !(0.0..=1.0).contains(edge))
            || crop.left >= crop.right
            || crop.top >= crop.bottom
        {
            return Err(ParseCropError);
        }

        Ok(crop)
    }
}

impl From<Crop> for ScreenshotCrop {
    fn from(crop: Crop) -> Self {
        Self {
            left: crop.left,
            top: crop.top,
            right: crop.right,
            bottom: crop.bottom,
        }
    }
}

#[derive(Serialize)]
struct ScreenshotOutput<'a> {
    path: &'a str,
    width: u32,
    height: u32,
    format: &'static str,
    warnings: &'a [String],
}

pub async fn run(target: Target, crop: Option<Crop>, destination: Option<PathBuf>) -> ExitCode {
    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let request = ScreenshotRequest::new(target.drawing_id, crop.map(Into::into));
    let screenshot = match client.screenshot(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            return fail(request_error_message(
                RequestOperation::Screenshot,
                Some(target),
                status,
            ));
        }
    };

    if screenshot.png.is_empty() || screenshot.width == 0 || screenshot.height == 0 {
        return fail("Invalid response: screenshot image is missing.".into());
    }

    let timestamp = timestamp(OffsetDateTime::now_utc());
    let path = match output::publish_png(&screenshot.png, destination.as_deref(), &timestamp) {
        Ok(path) => path,
        Err(error) => return fail(error.to_string()),
    };
    let Some(path) = path.to_str() else {
        return fail("Screenshot path is not valid UTF-8.".into());
    };
    let result = ScreenshotOutput {
        path,
        width: screenshot.width,
        height: screenshot.height,
        format: "png",
        warnings: &screenshot.warnings,
    };
    match serde_json::to_string(&result) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(_) => fail("Screenshot result could not be formatted.".into()),
    }
}

fn timestamp(now: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_normalized_crop() {
        assert_eq!(
            "0.2,0.1,0.8,0.65".parse(),
            Ok(Crop {
                left: 0.2,
                top: 0.1,
                right: 0.8,
                bottom: 0.65,
            })
        );
    }

    #[test]
    fn rejects_invalid_crop() {
        for crop in [
            "0,0,1",
            "0,0,1,1,1",
            "0,0,0,1",
            "0,0,1,0",
            "-0.1,0,1,1",
            "0,0,1.1,1",
            "NaN,0,1,1",
        ] {
            assert!(crop.parse::<Crop>().is_err(), "{crop}");
        }
    }

    #[test]
    fn formats_windows_safe_utc_timestamp() {
        let now = OffsetDateTime::from_unix_timestamp_nanos(1_787_094_896_123_000_000).unwrap();
        assert_eq!(timestamp(now), "20260818T231456.123Z");
    }
}
