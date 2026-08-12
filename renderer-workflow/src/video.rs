use std::{
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use fractal_renderer_core::VideoConfig;

/// A transport-independent FFmpeg image-sequence job shared by the CLI and
/// agent workflow. Frame selection and project authorization stay in their
/// respective adapters; process execution and atomic publication live here.
#[derive(Clone, Debug)]
pub struct VideoEncodeJob {
    pub ffmpeg: PathBuf,
    pub frames_directory: PathBuf,
    pub output_path: PathBuf,
    pub fps: u32,
    pub start_frame: u32,
    pub frame_count: u32,
    pub config: VideoConfig,
    pub overwrite: bool,
    /// Capture FFmpeg stderr here. `None` inherits the operator's stderr.
    pub diagnostic_log: Option<PathBuf>,
    /// Ask FFmpeg to print its normal progress statistics.
    pub show_progress: bool,
}

#[derive(Clone, Debug)]
pub struct VideoEncodeSummary {
    pub output_path: PathBuf,
    pub total_seconds: f64,
}

impl VideoEncodeJob {
    pub fn validate(&self, width: u32, height: u32) -> Result<()> {
        self.config.validate_dimensions(width, height)?;
        if self.fps == 0 || self.frame_count == 0 {
            bail!("video fps and frame_count must be greater than zero");
        }
        self.start_frame
            .checked_add(self.frame_count)
            .context("video frame range overflows u32")?;
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
                bail!("video output {} already exists", self.output_path.display());
            }
        }
        if let Some(log) = &self.diagnostic_log
            && log == &self.output_path
        {
            bail!("FFmpeg diagnostic log must differ from the video output");
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
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("FFmpeg version unknown")
            .to_owned())
    }

    pub fn encode(&self) -> Result<VideoEncodeSummary> {
        self.encode_with_cancel(&|| false)
    }

    pub fn encode_with_cancel(&self, cancelled: &dyn Fn() -> bool) -> Result<VideoEncodeSummary> {
        if cancelled() {
            bail!("encode cancelled before FFmpeg started");
        }
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
        if let Some(log) = &self.diagnostic_log
            && let Some(parent) = log.parent().filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("could not create FFmpeg log directory {}", parent.display())
            })?;
        }

        let temporary = temporary_video_path(&self.output_path)?;
        let stderr = match &self.diagnostic_log {
            Some(path) => Stdio::from(
                File::create(path)
                    .with_context(|| format!("could not create {}", path.display()))?,
            ),
            None => Stdio::inherit(),
        };
        let started = Instant::now();
        let child = Command::new(&self.ffmpeg)
            .args(self.arguments(&temporary))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                self.remove_diagnostic_log();
                return Err(error)
                    .with_context(|| format!("could not execute {}", self.ffmpeg.display()));
            }
        };
        loop {
            if let Some(status) = child
                .try_wait()
                .context("could not inspect FFmpeg status")?
            {
                if !status.success() {
                    let _ = fs::remove_file(&temporary);
                    let diagnostic = self.diagnostic();
                    let message = if diagnostic.is_empty() {
                        format!(
                            "FFmpeg failed with status {status}; PNG frames remain in {}",
                            self.frames_directory.display()
                        )
                    } else {
                        format!("FFmpeg failed with status {status}: {diagnostic}")
                    };
                    return Err(anyhow::anyhow!(message));
                }
                break;
            }
            if cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(&temporary);
                self.remove_diagnostic_log();
                bail!("encode cancelled while FFmpeg was running");
            }
            thread::sleep(Duration::from_millis(100));
        }
        publish_video(&temporary, &self.output_path, self.overwrite)?;
        self.remove_diagnostic_log();
        Ok(VideoEncodeSummary {
            output_path: self.output_path.clone(),
            total_seconds: started.elapsed().as_secs_f64(),
        })
    }

    fn arguments(&self, temporary: &Path) -> Vec<OsString> {
        let mut arguments = vec!["-hide_banner".into(), "-loglevel".into(), "warning".into()];
        if self.show_progress {
            arguments.push("-stats".into());
        }
        arguments.extend([
            "-y".into(),
            "-framerate".into(),
            self.fps.to_string().into(),
            "-start_number".into(),
            self.start_frame.to_string().into(),
            "-i".into(),
            self.frames_directory
                .join("frame_%06d.png")
                .into_os_string(),
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
        ]);
        if self.config.faststart {
            arguments.extend([OsString::from("-movflags"), OsString::from("+faststart")]);
        }
        arguments.push(temporary.as_os_str().to_owned());
        arguments
    }

    fn diagnostic(&self) -> String {
        self.diagnostic_log
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default()
            .trim()
            .chars()
            .take(2_000)
            .collect()
    }

    fn remove_diagnostic_log(&self) {
        if let Some(path) = &self.diagnostic_log {
            let _ = fs::remove_file(path);
        }
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

fn publish_video(temporary: &Path, output: &Path, overwrite: bool) -> Result<()> {
    if output.exists() && !overwrite {
        let _ = fs::remove_file(temporary);
        bail!("video output {} appeared while encoding", output.display());
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

    fn job() -> VideoEncodeJob {
        VideoEncodeJob {
            ffmpeg: "ffmpeg".into(),
            frames_directory: "frames with spaces".into(),
            output_path: "movie.mp4".into(),
            fps: 60,
            start_frame: 120,
            frame_count: 1_621,
            config: VideoConfig::default(),
            overwrite: false,
            diagnostic_log: None,
            show_progress: true,
        }
    }

    #[test]
    fn ffmpeg_arguments_preserve_paths_and_bound_the_frame_range() {
        let arguments = job().arguments(Path::new(".movie.123.tmp.mp4"));
        assert!(arguments.contains(&OsString::from("frames with spaces/frame_%06d.png")));
        let start = arguments
            .iter()
            .position(|argument| argument == "-start_number")
            .unwrap();
        assert_eq!(arguments[start + 1], "120");
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
    fn ffmpeg_failure_preserves_frames_and_an_existing_video() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "fractal-workflow-ffmpeg-{}-{nonce}",
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
        let job = VideoEncodeJob {
            ffmpeg: "ffmpeg".into(),
            frames_directory: directory.clone(),
            output_path: movie.clone(),
            fps: 2,
            start_frame: 0,
            frame_count: 3,
            config: VideoConfig::default(),
            overwrite: false,
            diagnostic_log: Some(directory.join("ffmpeg.log")),
            show_progress: false,
        };
        job.validate(64, 36).unwrap();
        assert!(job.ffmpeg_version().unwrap().starts_with("ffmpeg version"));
        job.encode().unwrap();
        let completed_movie = fs::read(&movie).unwrap();

        let failed_job = VideoEncodeJob {
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
