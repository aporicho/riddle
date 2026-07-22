//! Versioned device contract injected by the ReMagic runtime.
//!
//! QTFB v1 only returns a shared-memory key and byte length during its
//! initialization handshake.  It does not report the logical geometry or
//! pixel format, so applications must not infer those values from the buffer
//! size.  ReMagic owns device detection and supplies this fail-closed profile
//! before MagicPaper opens the hosted surface.

use std::io;

use serde::Deserialize;

pub(crate) const DEVICE_PROFILE_ENV: &str = "REMAGIC_DEVICE_PROFILE";
pub(crate) const PAPER_PRO_WIDTH: usize = 1620;
pub(crate) const PAPER_PRO_HEIGHT: usize = 2160;
pub(crate) const PAPER_PRO_QTFB_RGB565: u8 = 3;
pub(crate) const PAPER_PRO_MOVE_WIDTH: usize = 954;
pub(crate) const PAPER_PRO_MOVE_HEIGHT: usize = 1696;
pub(crate) const PAPER_PRO_MOVE_QTFB_RGB565: u8 = 6;

const PROFILE_SCHEMA_VERSION: u32 = 1;
const RGB565_BYTES_PER_PIXEL: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostedSurfaceSpec {
    pub qtfb_format: u8,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

impl HostedSurfaceSpec {
    pub fn required_bytes(self) -> io::Result<usize> {
        self.stride.checked_mul(self.height).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ReMagic device profile surface size overflows usize",
            )
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeviceProfileV1 {
    pub product: Product,
    pub codename: Codename,
    pub os_version: String,
    pub display: HostedSurfaceSpec,
    pub capabilities: Vec<String>,
}

impl DeviceProfileV1 {
    pub fn from_environment() -> io::Result<Self> {
        let value = std::env::var(DEVICE_PROFILE_ENV).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("managed MagicPaper requires {DEVICE_PROFILE_ENV}"),
            )
        })?;
        Self::parse(&value)
    }

    pub fn parse(value: &str) -> io::Result<Self> {
        let wire: WireProfile = serde_json::from_str(value).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid {DEVICE_PROFILE_ENV}: {error}"),
            )
        })?;
        wire.validate()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Product {
    PaperPro,
    PaperProMove,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Codename {
    Ferrari,
    Chiappa,
}

#[derive(Debug, Deserialize)]
struct WireProfile {
    schema_version: u32,
    product: Product,
    codename: Codename,
    os_version: String,
    display: WireDisplay,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WireDisplay {
    logical_width: usize,
    logical_height: usize,
    qtfb_format: u8,
    pixel_format: String,
    stride: usize,
}

impl WireProfile {
    fn validate(self) -> io::Result<DeviceProfileV1> {
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return invalid_profile(format!(
                "unsupported schema_version {}",
                self.schema_version
            ));
        }
        if self.os_version.trim().is_empty() {
            return invalid_profile("os_version is empty");
        }
        let expected = expected_surface(self.product, self.codename)?;
        let actual = HostedSurfaceSpec {
            qtfb_format: self.display.qtfb_format,
            width: self.display.logical_width,
            height: self.display.logical_height,
            stride: self.display.stride,
        };
        if self.display.pixel_format != "rgb565" {
            return invalid_profile(format!(
                "unsupported pixel_format {:?}",
                self.display.pixel_format
            ));
        }
        if actual != expected {
            return invalid_profile(format!(
                "device/display mismatch: expected {expected:?}, got {actual:?}"
            ));
        }
        actual.required_bytes()?;
        validate_capabilities(&self.capabilities)?;
        Ok(DeviceProfileV1 {
            product: self.product,
            codename: self.codename,
            os_version: self.os_version,
            display: actual,
            capabilities: self.capabilities,
        })
    }
}

fn expected_surface(product: Product, codename: Codename) -> io::Result<HostedSurfaceSpec> {
    let (width, height, qtfb_format) = match (product, codename) {
        (Product::PaperPro, Codename::Ferrari) => {
            (PAPER_PRO_WIDTH, PAPER_PRO_HEIGHT, PAPER_PRO_QTFB_RGB565)
        }
        (Product::PaperProMove, Codename::Chiappa) => (
            PAPER_PRO_MOVE_WIDTH,
            PAPER_PRO_MOVE_HEIGHT,
            PAPER_PRO_MOVE_QTFB_RGB565,
        ),
        _ => return invalid_profile("product and codename do not identify the same device"),
    };
    Ok(HostedSurfaceSpec {
        qtfb_format,
        width,
        height,
        stride: width * RGB565_BYTES_PER_PIXEL,
    })
}

fn validate_capabilities(capabilities: &[String]) -> io::Result<()> {
    for required in ["display:qtfb-v1", "input:pen-v1", "ink:direct-v1"] {
        if !capabilities.iter().any(|value| value == required) {
            return invalid_profile(format!("missing required capability {required}"));
        }
    }
    Ok(())
}

fn invalid_profile<T>(detail: impl std::fmt::Display) -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid {DEVICE_PROFILE_ENV}: {detail}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPABILITIES: &str =
        r#"["display:qtfb-v1","input:pen-v1","ink:direct-v1","lifecycle:v2"]"#;

    fn profile(product: &str, codename: &str, width: usize, height: usize, format: u8) -> String {
        format!(
            r#"{{"schema_version":1,"product":"{product}","codename":"{codename}","os_version":"3.27.0","display":{{"logical_width":{width},"logical_height":{height},"qtfb_format":{format},"pixel_format":"rgb565","stride":{}}},"capabilities":{CAPABILITIES}}}"#,
            width * RGB565_BYTES_PER_PIXEL
        )
    }

    #[test]
    fn accepts_both_supported_device_profiles() {
        let ferrari = DeviceProfileV1::parse(&profile(
            "paper_pro",
            "ferrari",
            PAPER_PRO_WIDTH,
            PAPER_PRO_HEIGHT,
            PAPER_PRO_QTFB_RGB565,
        ))
        .unwrap();
        assert_eq!(ferrari.product, Product::PaperPro);
        assert_eq!(ferrari.codename, Codename::Ferrari);
        assert_eq!(ferrari.display.stride, PAPER_PRO_WIDTH * 2);
        assert_eq!(
            ferrari.display.required_bytes().unwrap(),
            PAPER_PRO_WIDTH * PAPER_PRO_HEIGHT * 2
        );

        let chiappa = DeviceProfileV1::parse(&profile(
            "paper_pro_move",
            "chiappa",
            PAPER_PRO_MOVE_WIDTH,
            PAPER_PRO_MOVE_HEIGHT,
            PAPER_PRO_MOVE_QTFB_RGB565,
        ))
        .unwrap();
        assert_eq!(chiappa.product, Product::PaperProMove);
        assert_eq!(chiappa.codename, Codename::Chiappa);
        assert_eq!(chiappa.display.stride, PAPER_PRO_MOVE_WIDTH * 2);
        assert_eq!(
            chiappa.display.required_bytes().unwrap(),
            PAPER_PRO_MOVE_WIDTH * PAPER_PRO_MOVE_HEIGHT * 2
        );
    }

    #[test]
    fn rejects_crossed_identity_or_surface_contracts() {
        for invalid in [
            profile(
                "paper_pro",
                "chiappa",
                PAPER_PRO_WIDTH,
                PAPER_PRO_HEIGHT,
                PAPER_PRO_QTFB_RGB565,
            ),
            profile(
                "paper_pro",
                "ferrari",
                PAPER_PRO_MOVE_WIDTH,
                PAPER_PRO_MOVE_HEIGHT,
                PAPER_PRO_MOVE_QTFB_RGB565,
            ),
            profile(
                "paper_pro_move",
                "chiappa",
                PAPER_PRO_MOVE_WIDTH,
                PAPER_PRO_MOVE_HEIGHT,
                PAPER_PRO_QTFB_RGB565,
            ),
        ] {
            assert!(DeviceProfileV1::parse(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn rejects_incomplete_or_future_contracts() {
        let valid = profile(
            "paper_pro_move",
            "chiappa",
            PAPER_PRO_MOVE_WIDTH,
            PAPER_PRO_MOVE_HEIGHT,
            PAPER_PRO_MOVE_QTFB_RGB565,
        );
        for invalid in [
            valid.replace("\"schema_version\":1", "\"schema_version\":2"),
            valid.replace("\"os_version\":\"3.27.0\"", "\"os_version\":\"\""),
            valid.replace(
                "\"pixel_format\":\"rgb565\"",
                "\"pixel_format\":\"rgba8888\"",
            ),
            valid.replace(
                "\"display:qtfb-v1\",\"input:pen-v1\",\"ink:direct-v1\",",
                "",
            ),
            valid.replace("\"stride\":1908", "\"stride\":1909"),
        ] {
            assert!(DeviceProfileV1::parse(&invalid).is_err(), "{invalid}");
        }
    }
}
