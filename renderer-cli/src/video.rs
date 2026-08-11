use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::VideoConfig;

#[derive(Debug)]
pub struct VideoJob {
    pub ffmpeg: PathBuf,
    pub frames_directory: PathBuf,
    pub output_path: PathBuf,
    pub fps: u32,
    pub frame_count: u32,
    pub config: VideoConfig,
    pub overwrite: bool,
}

impl VideoJob {
    pub fn validate(&self, width: u32, height: u32) -> Result<()> {
        self.config.validate_dimensions(width, height)?;
        if self.fps == 0 || self.frame_count == 0 {
            bail!("video fps and frame_count must be greater than zero");
        }
        if self.output_path.extension().is_none() {
            bail!(
                "video output {} must have a container extension such as .mp4",
                self.output_path.display()
            );
        }
        if self.output_path.exists() {
            if !self.output_path.is_file() {
                bail!("video output {} is not a file", self.output_path.display());
            }
            if !self.overwrite {
                bail!(
                    "video output {} already exists; pass --video-overwrite (or --overwrite) to replace it",
                    self.output_path.display()
                );
            }
        }
        Ok(())
    }

    /// Checks the executable before a long render and returns its version line.
    pub fn ffmpeg_version(&self) -> Result<String> {
        let output = Command::new(&self.ffmpeg)
            .arg("-version")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("could not execute {}", self.ffmpeg.display()))?;
        if !output.status.success() {
            bail!(
                "{} -version failed with status {}",
                self.ffmpeg.display(),
                output.status
            );
        }
        let first_line = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("FFmpeg version unknown")
            .to_owned();
        Ok(first_line)
    }

    /// Encodes a complete image sequence. PNG inputs are never removed.
    pub fn encode(&self) -> Result<()> {
        if let Some(parent) = self
            .output_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "could not create video output directory {}",
                    parent.display()
                )
            })?;
        }
        let temporary_path = temporary_video_path(&self.output_path)?;
        let status = Command::new(&self.ffmpeg)
            .args(self.arguments(&temporary_path))
            .stdin(Stdio::null())
            .status();
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                let _ = fs::remove_file(&temporary_path);
                return Err(error)
                    .with_context(|| format!("could not execute {}", self.ffmpeg.display()));
            }
        };
        if !status.success() {
            let _ = fs::remove_file(&temporary_path);
            bail!(
                "FFmpeg failed with status {status}; PNG frames remain in {}",
                self.frames_directory.display()
            );
        }
        move_completed_video(&temporary_path, &self.output_path, self.overwrite)?;
        Ok(())
    }

    fn arguments(&self, temporary_path: &Path) -> Vec<OsString> {
        let input_pattern = self.frames_directory.join("frame_%06d.png");
        let mut arguments = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "warning".into(),
            "-stats".into(),
            "-y".into(),
            "-framerate".into(),
            self.fps.to_string().into(),
            "-start_number".into(),
            "0".into(),
            "-i".into(),
            input_pattern.into_os_string(),
            "-frames:v".into(),
            self.frame_count.to_string().into(),
            "-an".into(),
            "-c:v".into(),
            self.config.codec.clone().into(),
            "-pix_fmt".into(),
            self.config.pixel_format.clone().into(),
            "-crf".into(),
            self.config.crf.to_string().into(),
            "-preset".into(),
            self.config.preset.clone().into(),
        ];
        if self.config.faststart {
            arguments.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
        }
        arguments.push(temporary_path.as_os_str().to_owned());
        arguments
    }
}

fn temporary_video_path(output_path: &Path) -> Result<PathBuf> {
    let stem = output_path
        .file_stem()
        .context("video output path must end in a file name")?
        .to_string_lossy();
    let extension = output_path
        .extension()
        .context("video output path must have a container extension")?
        .to_string_lossy();
    Ok(output_path.with_file_name(format!(".{stem}.{}.tmp.{extension}", std::process::id())))
}

fn move_completed_video(temporary: &Path, output: &Path, overwrite: bool) -> Result<()> {
    if output.exists() && !overwrite {
        let _ = fs::remove_file(temporary);
        bail!(
            "video output {} appeared while encoding; pass --video-overwrite (or --overwrite) to replace it",
            output.display()
        );
    }
    let mut moved = fs::rename(temporary, output);
    if moved.is_err() && overwrite && output.is_file() {
        fs::remove_file(output)
            .with_context(|| format!("could not replace existing video {}", output.display()))?;
        moved = fs::rename(temporary, output);
    }
    if let Err(error) = moved {
        let _ = fs::remove_file(temporary);
        return Err(error).context("could not move the completed video into place");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn ffmpeg_arguments_preserve_paths_and_bound_the_frame_count() {
        let job = VideoJob {
            ffmpeg: "ffmpeg".into(),
            frames_directory: "frames with spaces".into(),
            output_path: "movie.mp4".into(),
            fps: 60,
            frame_count: 1_621,
            config: VideoConfig::default(),
            overwrite: false,
        };
        let arguments = job.arguments(Path::new(".movie.123.tmp.mp4"));
        assert!(arguments.contains(&OsString::from("frames with spaces/frame_%06d.png")));
        let frame_limit = arguments
            .iter()
            .position(|argument| argument == "-frames:v")
            .unwrap();
        assert_eq!(arguments[frame_limit + 1], "1621");
        assert_eq!(arguments.last().unwrap(), ".movie.123.tmp.mp4");
    }

    #[test]
    fn temporary_output_keeps_the_container_extension() {
        let temporary = temporary_video_path(Path::new("output/movie.mp4")).unwrap();
        assert_eq!(temporary.extension().unwrap(), "mp4");
        assert!(
            temporary
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".tmp.")
        );
    }

    #[test]
    #[ignore = "requires FFmpeg"]
    fn ffmpeg_encodes_sequence_and_failure_preserves_png_frames() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fractal-renderer-ffmpeg-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        for frame_index in 0..3_u32 {
            let channel = (frame_index * 90) as u8;
            let mut pixels = vec![0_u8; 64 * 36 * 4];
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[channel, 255 - channel, 96, 255]);
            }
            image::save_buffer_with_format(
                directory.join(format!("frame_{frame_index:06}.png")),
                &pixels,
                64,
                36,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            )
            .unwrap();
        }

        let movie = directory.join("movie.mp4");
        let job = VideoJob {
            ffmpeg: "ffmpeg".into(),
            frames_directory: directory.clone(),
            output_path: movie.clone(),
            fps: 2,
            frame_count: 3,
            config: VideoConfig::default(),
            overwrite: false,
        };
        job.validate(64, 36).unwrap();
        assert!(job.ffmpeg_version().unwrap().starts_with("ffmpeg version"));
        job.encode().unwrap();
        assert!(fs::metadata(&movie).unwrap().len() > 0);
        let completed_movie = fs::read(&movie).unwrap();

        let failed_job = VideoJob {
            output_path: movie.clone(),
            config: VideoConfig {
                codec: "not_a_real_encoder".to_owned(),
                ..VideoConfig::default()
            },
            overwrite: true,
            ..job
        };
        assert!(failed_job.encode().is_err());
        assert_eq!(fs::read(&movie).unwrap(), completed_movie);
        for frame_index in 0..3_u32 {
            assert!(
                directory
                    .join(format!("frame_{frame_index:06}.png"))
                    .exists()
            );
        }

        fs::remove_dir_all(directory).unwrap();
    }
}
