use std::{ffi::OsString, fs, path::PathBuf};

use macroquad::texture::Image;

#[derive(Debug, PartialEq, Eq)]
pub struct Capture {
    pub path: PathBuf,
    /// One-based rendered frame number, captured after drawing and before presenting.
    pub frame: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PlayerOptions {
    pub width: u16,
    pub height: u16,
    pub capture: Option<Capture>,
}

impl PlayerOptions {
    pub fn from_env() -> Result<Self, String> {
        Self::read(|name| std::env::var_os(name))
    }

    fn read(mut variable: impl FnMut(&str) -> Option<OsString>) -> Result<Self, String> {
        let width = dimension("PLAYER_WIDTH", variable("PLAYER_WIDTH"), 960)?;
        let height = dimension("PLAYER_HEIGHT", variable("PLAYER_HEIGHT"), 600)?;
        let path = variable("PLAYER_CAPTURE");
        let frame = variable("PLAYER_CAPTURE_FRAME");
        let capture = match path {
            Some(path) if path.is_empty() => {
                return Err("PLAYER_CAPTURE must be a nonempty output path".to_owned());
            }
            Some(path) => Some(Capture {
                path: PathBuf::from(path),
                frame: positive_number("PLAYER_CAPTURE_FRAME", frame, 4)?,
            }),
            None if frame.is_some() => {
                return Err("PLAYER_CAPTURE_FRAME requires PLAYER_CAPTURE".to_owned());
            }
            None => None,
        };
        Ok(Self {
            width,
            height,
            capture,
        })
    }
}

fn positive_number(name: &str, value: Option<OsString>, default: u32) -> Result<u32, String> {
    match value {
        None => Ok(default),
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&value| value > 0)
            .ok_or_else(|| format!("{name} must be a positive integer up to {}", u32::MAX)),
    }
}

fn dimension(name: &str, value: Option<OsString>, default: u32) -> Result<u16, String> {
    u16::try_from(positive_number(name, value, default)?)
        .map_err(|_| format!("{name} must be at most {}", u16::MAX))
}

pub fn save_png(screenshot: Image, path: &std::path::Path) -> Result<(), String> {
    let save = || -> Result<(), Box<dyn std::error::Error>> {
        let mut pixels = image::RgbaImage::from_raw(
            u32::from(screenshot.width),
            u32::from(screenshot.height),
            screenshot.bytes,
        )
        .ok_or("invalid screenshot pixel buffer")?;
        // Macroquad's framebuffer readback is bottom-up, like Image::export_png.
        image::imageops::flip_vertical_in_place(&mut pixels);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        // Explicit PNG encoding preserves the format even without a .png extension.
        pixels.save_with_format(path, image::ImageFormat::Png)?;
        Ok(())
    };
    save().map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(values: &[(&str, &str)]) -> Result<PlayerOptions, String> {
        PlayerOptions::read(|name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn no_configuration_preserves_interactive_defaults() {
        assert_eq!(
            options(&[]).unwrap(),
            PlayerOptions {
                width: 960,
                height: 600,
                capture: None,
            }
        );
    }

    #[test]
    fn capture_defaults_to_fourth_frame_and_accepts_unicode_paths() {
        let options = options(&[("PLAYER_CAPTURE", "snapshots/My game – 雪.png")]).unwrap();
        let capture = options.capture.unwrap();
        assert_eq!(capture.frame, 4);
        assert_eq!(capture.path, PathBuf::from("snapshots/My game – 雪.png"));
    }

    #[test]
    fn dimensions_and_capture_frame_can_be_overridden() {
        let options = options(&[
            ("PLAYER_WIDTH", "400"),
            ("PLAYER_HEIGHT", "260"),
            ("PLAYER_CAPTURE", "small.png"),
            ("PLAYER_CAPTURE_FRAME", "1"),
        ])
        .unwrap();
        assert_eq!((options.width, options.height), (400, 260));
        assert_eq!(options.capture.unwrap().frame, 1);
    }

    #[test]
    fn malformed_configuration_is_an_error_instead_of_a_silent_default() {
        for (name, value) in [
            ("PLAYER_WIDTH", "0"),
            ("PLAYER_WIDTH", "65536"),
            ("PLAYER_HEIGHT", "-1"),
            ("PLAYER_HEIGHT", "wide"),
            ("PLAYER_CAPTURE_FRAME", "0"),
            ("PLAYER_CAPTURE_FRAME", "4294967296"),
            ("PLAYER_CAPTURE", ""),
        ] {
            let error = options(&[(name, value), ("PLAYER_CAPTURE", "capture.png")])
                .expect_err("invalid configuration must fail");
            assert!(error.contains(name), "{error}");
        }
        assert!(options(&[("PLAYER_CAPTURE_FRAME", "4")]).is_err());
    }

    fn two_rows() -> Image {
        Image {
            width: 1,
            height: 2,
            // Bottom red, top blue in framebuffer order.
            bytes: vec![255, 0, 0, 255, 0, 0, 255, 255],
        }
    }

    #[test]
    fn png_has_correct_orientation_dimensions_and_creates_parent_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/capture.png");
        save_png(two_rows(), &path).unwrap();

        let png = image::open(path).unwrap().into_rgba8();
        assert_eq!(png.dimensions(), (1, 2));
        assert_eq!(png.get_pixel(0, 0).0, [0, 0, 255, 255]);
        assert_eq!(png.get_pixel(0, 1).0, [255, 0, 0, 255]);
    }

    #[test]
    fn unwritable_output_returns_an_error_instead_of_panicking() {
        let directory = tempfile::tempdir().unwrap();
        assert!(save_png(two_rows(), directory.path()).is_err());
    }
}
