mod output;

use std::fmt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use acadctl_rpc::{ScreenshotRegion, ScreenshotRequest};
use serde::Serialize;
use time::OffsetDateTime;

use super::{RequestOperation, fail, request_error_message, target::Target};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Region {
    min_x: f64,
    min_y: f64,
    max_x: f64,
    max_y: f64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParseRegionError;

impl fmt::Display for ParseRegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected two distinct finite X,Y corners separated by ':'")
    }
}

impl std::error::Error for ParseRegionError {}

impl FromStr for Region {
    type Err = ParseRegionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut corners = value.split(':');
        let first = parse_corner(corners.next().ok_or(ParseRegionError)?)?;
        let second = parse_corner(corners.next().ok_or(ParseRegionError)?)?;
        if corners.next().is_some() || first.0 == second.0 || first.1 == second.1 {
            return Err(ParseRegionError);
        }

        Ok(Self {
            min_x: first.0.min(second.0),
            min_y: first.1.min(second.1),
            max_x: first.0.max(second.0),
            max_y: first.1.max(second.1),
        })
    }
}

fn parse_corner(value: &str) -> Result<(f64, f64), ParseRegionError> {
    let mut coordinates = value.split(',').map(str::parse::<f64>);
    let x = coordinates
        .next()
        .ok_or(ParseRegionError)?
        .map_err(|_| ParseRegionError)?;
    let y = coordinates
        .next()
        .ok_or(ParseRegionError)?
        .map_err(|_| ParseRegionError)?;
    if coordinates.next().is_some() || !x.is_finite() || !y.is_finite() {
        return Err(ParseRegionError);
    }

    Ok((x, y))
}

impl From<Region> for ScreenshotRegion {
    fn from(region: Region) -> Self {
        Self {
            min_x: region.min_x,
            min_y: region.min_y,
            max_x: region.max_x,
            max_y: region.max_y,
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

pub async fn run(
    target: Target,
    region: Region,
    wide: bool,
    destination: Option<PathBuf>,
) -> ExitCode {
    let mut client = match super::connect_drawings(target.instance_id).await {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let request = ScreenshotRequest::new(target.drawing_id, region.into(), wide);
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
    fn parses_and_normalizes_wcs_region() {
        assert_eq!(
            "1e1,20:-1e2,-2.5e1".parse(),
            Ok(Region {
                min_x: -100.0,
                min_y: -25.0,
                max_x: 10.0,
                max_y: 20.0,
            })
        );
    }

    #[test]
    fn rejects_invalid_regions() {
        for region in [
            "0,0",
            "0,0:1",
            "0,0:1,1:2,2",
            "0,0,0:1,1",
            "0,0:0,1",
            "0,0:1,0",
            "NaN,0:1,1",
            "0,inf:1,1",
        ] {
            assert!(region.parse::<Region>().is_err(), "{region}");
        }
    }

    #[test]
    fn converts_region_to_rpc_coordinates() {
        let region: ScreenshotRegion = "10,20:-100,-25".parse::<Region>().unwrap().into();

        assert_eq!(
            region,
            ScreenshotRegion {
                min_x: -100.0,
                min_y: -25.0,
                max_x: 10.0,
                max_y: 20.0,
            }
        );
    }

    #[test]
    fn formats_windows_safe_utc_timestamp() {
        let now = OffsetDateTime::from_unix_timestamp_nanos(1_787_094_896_123_000_000).unwrap();
        assert_eq!(timestamp(now), "20260818T231456.123Z");
    }
}
