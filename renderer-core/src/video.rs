use anyhow::{Result, bail};

const MAX_FFMPEG_VALUE_LENGTH: usize = 64;
pub const MAX_VIDEO_CRF: u8 = 63;

/// Codec settings consumed by the CLI's FFmpeg integration.
///
/// The core crate owns validation and scene representation, while subprocess
/// execution remains outside the GPU renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoConfig {
    pub codec: String,
    pub pixel_format: String,
    pub crf: u8,
    pub preset: String,
    pub faststart: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            codec: "libx264".to_owned(),
            pixel_format: "yuv420p".to_owned(),
            crf: 18,
            preset: "slow".to_owned(),
            faststart: true,
        }
    }
}

impl VideoConfig {
    pub fn validate(&self) -> Result<()> {
        validate_ffmpeg_value("video codec", &self.codec)?;
        validate_ffmpeg_value("video pixel_format", &self.pixel_format)?;
        validate_ffmpeg_value("video preset", &self.preset)?;
        if self.crf > MAX_VIDEO_CRF {
            bail!("video crf must be in 0..={MAX_VIDEO_CRF}");
        }
        if matches!(self.codec.as_str(), "libx264" | "libx265") && self.crf > 51 {
            bail!("{} crf must be in 0..=51", self.codec);
        }
        Ok(())
    }

    pub fn validate_dimensions(&self, width: u32, height: u32) -> Result<()> {
        self.validate()?;
        if self.pixel_format.starts_with("yuv420p")
            && (!width.is_multiple_of(2) || !height.is_multiple_of(2))
        {
            bail!(
                "{} video requires even image width and height, got {width}x{height}",
                self.pixel_format
            );
        }
        Ok(())
    }
}

fn validate_ffmpeg_value(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_FFMPEG_VALUE_LENGTH {
        bail!("{label} must contain 1..={MAX_FFMPEG_VALUE_LENGTH} characters");
    }
    if !value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
    {
        bail!("{label} must start with an ASCII letter or digit");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
    {
        bail!("{label} may contain only ASCII letters, digits, '-', '_', '.', and '+'");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_h264_settings_are_valid() {
        let config = VideoConfig::default();
        config.validate_dimensions(1_920, 1_080).unwrap();
    }

    #[test]
    fn rejects_unsafe_values_and_odd_yuv420p_dimensions() {
        let config = VideoConfig {
            codec: "libx264;touch".to_owned(),
            ..VideoConfig::default()
        };
        assert!(config.validate().is_err());

        let config = VideoConfig {
            codec: "-y".to_owned(),
            ..VideoConfig::default()
        };
        assert!(config.validate().is_err());

        let config = VideoConfig::default();
        assert!(config.validate_dimensions(641, 360).is_err());

        let config = VideoConfig {
            crf: 52,
            ..VideoConfig::default()
        };
        assert!(config.validate().is_err());
    }
}
