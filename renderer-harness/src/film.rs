//! Assembles cuts into one film: applies the transitions between cuts and
//! streams the result to FFmpeg. The animatic and the final encode share
//! this path, so what is reviewed is cut together the way it will ship.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use anyhow::{Context, Result, bail};

use crate::shotlist::Transition;

/// Applies fades and crossfades to a stream of cuts and forwards finished
/// frames, as tightly packed RGBA8, to a sink.
pub struct Compositor<S: FnMut(&[u8]) -> Result<()>> {
    sink: S,
    transition_frames: usize,
    /// How the cut being pushed begins: the previous cut's transition.
    incoming: Transition,
    outgoing: Transition,
    cut_frames: usize,
    pushed: usize,
    /// Tail of the previous cut, waiting to be blended into this one.
    held: Vec<Vec<u8>>,
    emitted: usize,
}

impl<S: FnMut(&[u8]) -> Result<()>> Compositor<S> {
    pub fn new(transition_frames: usize, sink: S) -> Self {
        Self {
            sink,
            transition_frames,
            incoming: Transition::Cut,
            outgoing: Transition::Cut,
            cut_frames: 0,
            pushed: 0,
            held: Vec::new(),
            emitted: 0,
        }
    }

    /// Starts the next cut. Every frame of the previous cut must have been
    /// pushed.
    pub fn begin_cut(&mut self, frames: usize, transition_out: Transition) -> Result<()> {
        if self.pushed != self.cut_frames {
            bail!(
                "the previous cut received {} of its {} frames",
                self.pushed,
                self.cut_frames
            );
        }
        let has_transition = transition_out != Transition::Cut || self.outgoing != Transition::Cut;
        if has_transition && frames < 2 * self.transition_frames {
            bail!(
                "a cut of {frames} frames is too short for transitions of {} frames at both ends",
                self.transition_frames
            );
        }
        self.incoming = self.outgoing;
        self.outgoing = transition_out;
        self.cut_frames = frames;
        self.pushed = 0;
        Ok(())
    }

    pub fn push_frame(&mut self, mut rgba: Vec<u8>) -> Result<()> {
        if self.pushed >= self.cut_frames {
            bail!("the cut received more than its {} frames", self.cut_frames);
        }
        let index = self.pushed;
        self.pushed += 1;
        let span = self.transition_frames;

        if index < span {
            match self.incoming {
                Transition::Cut => {}
                // Ramps exclude both ends: the first frame is already
                // partly visible and the last is not yet fully so.
                Transition::FadeBlack => scale(&mut rgba, ramp(index, span)),
                Transition::Crossfade => {
                    let from = &self.held[index];
                    if from.len() != rgba.len() {
                        bail!("cuts of different frame sizes cannot be crossfaded");
                    }
                    blend(&mut rgba, from, ramp(index, span));
                }
            }
            if index + 1 == span {
                self.held.clear();
            }
        }

        let remaining = self.cut_frames - 1 - index;
        if remaining < span {
            match self.outgoing {
                Transition::Cut => {}
                Transition::FadeBlack => scale(&mut rgba, ramp(remaining, span)),
                Transition::Crossfade => {
                    self.held.push(rgba);
                    return Ok(());
                }
            }
        }
        self.emit(&rgba)
    }

    fn emit(&mut self, rgba: &[u8]) -> Result<()> {
        self.emitted += 1;
        (self.sink)(rgba)
    }

    /// Ends the film and returns the number of frames emitted.
    pub fn finish(self) -> Result<usize> {
        if self.pushed != self.cut_frames {
            bail!(
                "the last cut received {} of its {} frames",
                self.pushed,
                self.cut_frames
            );
        }
        if !self.held.is_empty() {
            bail!("the film ends in a crossfade with nothing to fade into");
        }
        Ok(self.emitted)
    }
}

/// Opacity of frame `index` within a transition of `span` frames.
fn ramp(index: usize, span: usize) -> f32 {
    (index + 1) as f32 / (span + 1) as f32
}

fn scale(rgba: &mut [u8], opacity: f32) {
    for pixel in rgba.chunks_exact_mut(4) {
        for channel in &mut pixel[..3] {
            *channel = (f32::from(*channel) * opacity).round() as u8;
        }
    }
}

/// Mixes `rgba` over `from` with the given opacity.
fn blend(rgba: &mut [u8], from: &[u8], opacity: f32) {
    for (pixel, from) in rgba.chunks_exact_mut(4).zip(from.chunks_exact(4)) {
        for channel in 0..3 {
            pixel[channel] = (f32::from(pixel[channel]) * opacity
                + f32::from(from[channel]) * (1.0 - opacity))
                .round() as u8;
        }
    }
}

/// Encoder settings for a film file.
#[derive(Clone, Debug)]
pub struct Encoding {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub rate: Rate,
    pub preset: &'static str,
}

/// How the encoder trades size for quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rate {
    /// Constant quality; lower is better.
    Crf(u32),
    /// A fixed average bitrate, as a delivery platform would transcode to.
    Kbps(u32),
}

/// Streams raw frames into an FFmpeg process that writes an MP4.
pub struct FilmWriter {
    child: Child,
    frame_bytes: usize,
    temporary: PathBuf,
    target: PathBuf,
}

impl FilmWriter {
    pub fn create(path: &Path, encoding: &Encoding) -> Result<Self> {
        if !encoding.width.is_multiple_of(2) || !encoding.height.is_multiple_of(2) {
            bail!(
                "yuv420p needs even dimensions, not {}x{}",
                encoding.width,
                encoding.height
            );
        }
        let temporary = path.with_extension(format!("tmp-{}.mp4", std::process::id()));
        let child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{}x{}", encoding.width, encoding.height)])
            .args(["-r", &encoding.fps.to_string(), "-i", "-"])
            // YouTube's recommended H.264 structure: closed GOP of half the
            // frame rate, two B-frames, 4:2:0, moov atom up front.
            .args([
                "-c:v",
                "libx264",
                "-profile:v",
                "high",
                "-pix_fmt",
                "yuv420p",
            ])
            .args(["-preset", encoding.preset])
            .args(match encoding.rate {
                Rate::Crf(crf) => vec!["-crf".to_owned(), crf.to_string()],
                Rate::Kbps(kbps) => vec![
                    "-b:v".to_owned(),
                    format!("{kbps}k"),
                    "-maxrate".to_owned(),
                    format!("{}k", kbps * 3 / 2),
                    "-bufsize".to_owned(),
                    format!("{}k", kbps * 2),
                ],
            })
            .args(["-g", &(encoding.fps / 2).max(1).to_string(), "-bf", "2"])
            .args(["-flags", "+cgop", "-movflags", "+faststart"])
            // Frames are sRGB; tag and convert to BT.709 limited range.
            .args([
                "-vf",
                "scale=in_range=full:out_range=tv:out_color_matrix=bt709",
            ])
            .args(["-color_primaries", "bt709", "-color_trc", "bt709"])
            .args(["-colorspace", "bt709", "-color_range", "tv"])
            .arg(&temporary)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not start ffmpeg; is it installed and on PATH?")?;
        Ok(Self {
            child,
            frame_bytes: encoding.width as usize * encoding.height as usize * 4,
            temporary,
            target: path.to_owned(),
        })
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        if rgba.len() != self.frame_bytes {
            bail!(
                "frame of {} bytes does not match the film raster of {} bytes",
                rgba.len(),
                self.frame_bytes
            );
        }
        let result = self
            .child
            .stdin
            .as_mut()
            .context("ffmpeg input is closed")?
            .write_all(rgba);
        if result.is_err() {
            // FFmpeg exited early; its own message explains why.
            return Err(self.failure());
        }
        Ok(())
    }

    /// Closes the stream and moves the finished file into place.
    pub fn finish(mut self) -> Result<()> {
        drop(self.child.stdin.take());
        let output = self
            .child
            .wait_with_output()
            .context("could not wait for ffmpeg")?;
        if !output.status.success() {
            let _ = std::fs::remove_file(&self.temporary);
            bail!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        std::fs::rename(&self.temporary, &self.target)
            .with_context(|| format!("could not write {}", self.target.display()))
    }

    fn failure(&mut self) -> anyhow::Error {
        drop(self.child.stdin.take());
        let mut message = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut message);
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.temporary);
        anyhow::anyhow!("ffmpeg stopped accepting frames: {}", message.trim())
    }
}

/// Decodes a video file and hands every frame, as tightly packed RGBA8, to
/// `sink` together with its index. Returns the number of frames.
pub fn decode_frames(
    path: &Path,
    width: u32,
    height: u32,
    mut sink: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<usize> {
    use std::io::Read;
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        // Undo the encoder's BT.709 limited-range conversion.
        .args([
            "-vf",
            "scale=in_range=tv:out_range=full:in_color_matrix=bt709",
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start ffmpeg; is it installed and on PATH?")?;
    let mut stdout = child.stdout.take().context("ffmpeg output is closed")?;
    let mut frame = vec![0u8; width as usize * height as usize * 4];
    let mut count = 0;
    let outcome = loop {
        match stdout.read_exact(&mut frame) {
            Ok(()) => {
                if let Err(error) = sink(count, &frame) {
                    break Err(error);
                }
                count += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(error) => break Err(error).context("could not read decoded frames"),
        }
    };
    drop(stdout);
    let status = child.wait().context("could not wait for ffmpeg")?;
    outcome?;
    if !status.success() || count == 0 {
        bail!("ffmpeg could not decode {}", path.display());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> Vec<u8> {
        vec![value, value, value, 255]
    }

    /// Runs cuts of constant brightness through a compositor and returns
    /// the red channel of every emitted frame.
    fn run(span: usize, cuts: &[(u8, usize, Transition)]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut compositor = Compositor::new(span, |rgba: &[u8]| {
            out.push(rgba[0]);
            // Alpha must survive every transition untouched.
            assert_eq!(rgba[3], 255);
            Ok(())
        });
        for (value, frames, transition) in cuts {
            compositor.begin_cut(*frames, *transition)?;
            for _ in 0..*frames {
                compositor.push_frame(frame(*value))?;
            }
        }
        let emitted = compositor.finish()?;
        assert_eq!(emitted, out.len());
        Ok(out)
    }

    #[test]
    fn hard_cuts_pass_frames_through() {
        let out = run(2, &[(10, 3, Transition::Cut), (200, 2, Transition::Cut)]).unwrap();
        assert_eq!(out, vec![10, 10, 10, 200, 200]);
    }

    #[test]
    fn fade_through_black_dims_both_sides_and_keeps_length() {
        let out = run(
            2,
            &[(90, 5, Transition::FadeBlack), (150, 5, Transition::Cut)],
        )
        .unwrap();
        assert_eq!(out, vec![90, 90, 90, 60, 30, 50, 100, 150, 150, 150]);
    }

    #[test]
    fn crossfade_overlaps_the_cuts_and_shortens_the_film() {
        let out = run(
            2,
            &[(0, 4, Transition::Crossfade), (90, 4, Transition::Cut)],
        )
        .unwrap();
        // Two frames of each cut merge into two blended frames.
        assert_eq!(out, vec![0, 0, 30, 60, 90, 90]);
    }

    #[test]
    fn a_cut_can_fade_in_and_cross_out() {
        let out = run(
            1,
            &[
                (100, 2, Transition::FadeBlack),
                (100, 3, Transition::Crossfade),
                (200, 2, Transition::Cut),
            ],
        )
        .unwrap();
        assert_eq!(out, vec![100, 50, 50, 100, 150, 200]);
    }

    #[test]
    fn misuse_is_reported() {
        // Too short for its transitions.
        assert!(run(3, &[(1, 4, Transition::FadeBlack), (1, 9, Transition::Cut)]).is_err());
        // A crossfade needs a following cut.
        assert!(run(1, &[(1, 4, Transition::Crossfade)]).is_err());

        let mut compositor = Compositor::new(1, |_: &[u8]| Ok(()));
        compositor.begin_cut(2, Transition::Cut).unwrap();
        compositor.push_frame(frame(1)).unwrap();
        assert!(compositor.begin_cut(2, Transition::Cut).is_err());
        compositor.push_frame(frame(1)).unwrap();
        assert!(compositor.push_frame(frame(1)).is_err());
    }

    #[test]
    fn writer_encodes_a_playable_file() {
        let directory =
            std::env::temp_dir().join(format!("fractal-harness-film-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("clip.mp4");
        let encoding = Encoding {
            width: 64,
            height: 36,
            fps: 30,
            rate: Rate::Crf(30),
            preset: "ultrafast",
        };
        let mut writer = FilmWriter::create(&path, &encoding).unwrap();
        assert!(writer.write_frame(&[0; 16]).is_err());

        let mut writer = FilmWriter::create(&path, &encoding).unwrap();
        for index in 0..12u8 {
            writer
                .write_frame(&[index * 20, 0, 0, 255].repeat(64 * 36))
                .unwrap();
        }
        writer.finish().unwrap();

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
            .args(["-show_entries", "stream=nb_read_frames,pix_fmt,color_space"])
            .args(["-of", "csv=p=0"])
            .arg(&path)
            .output()
            .unwrap();
        let description = String::from_utf8_lossy(&probe.stdout);
        assert_eq!(description.trim(), "yuv420p,bt709,12", "{description}");
        // No temporary file may be left next to the result.
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);

        // The file decodes back to the frames that went in, within what
        // lossy 4:2:0 coding preserves.
        let mut reds = Vec::new();
        let decoded = decode_frames(&path, 64, 36, |index, rgba| {
            assert_eq!(rgba.len(), 64 * 36 * 4);
            assert_eq!(index, reds.len());
            reds.push(rgba[0]);
            Ok(())
        })
        .unwrap();
        assert_eq!(decoded, 12);
        for (index, red) in reds.iter().enumerate() {
            let expected = index as i32 * 20;
            assert!(
                (i32::from(*red) - expected).abs() <= 12,
                "frame {index}: {red}"
            );
        }
        assert!(decode_frames(&directory.join("missing.mp4"), 64, 36, |_, _| Ok(())).is_err());

        // A bitrate target produces a smaller file than near-lossless CRF.
        let mut rng = crate::rng::Rng::new(1);
        let noise: Vec<Vec<u8>> = (0..24)
            .map(|_| {
                (0..64 * 36)
                    .flat_map(|_| {
                        let value = rng.below(256) as u8;
                        [value, value, value, 255]
                    })
                    .collect()
            })
            .collect();
        let size_with = |rate: Rate, name: &str| {
            let path = directory.join(name);
            let mut writer = FilmWriter::create(
                &path,
                &Encoding {
                    rate,
                    ..encoding.clone()
                },
            )
            .unwrap();
            for frame in &noise {
                writer.write_frame(frame).unwrap();
            }
            writer.finish().unwrap();
            std::fs::metadata(&path).unwrap().len()
        };
        assert!(size_with(Rate::Kbps(40), "low.mp4") < size_with(Rate::Crf(4), "high.mp4") / 2);

        assert!(
            FilmWriter::create(
                &path,
                &Encoding {
                    width: 63,
                    ..encoding
                }
            )
            .is_err()
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
