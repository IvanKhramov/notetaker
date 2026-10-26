use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, ensure};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus_rs::{Application, OpusEncoder};

use crate::{AudioFrame, FRAME_SAMPLES, SAMPLE_RATE, sample_position};

// VoIP at 16 kHz uses SILK wideband: 6.5 ms algorithmic lookahead (104
// input samples). Ogg Opus always expresses pre-skip/granules at 48 kHz.
const PRE_SKIP: usize = 104;
const SERIAL: u32 = 1;

/// Ogg Opus, 16 kHz mono input, 24 kbit/s. The encoder and muxer are pure Rust:
/// MSVC builds need neither a system libopus nor a C cross toolchain.
/// Call `finish` explicitly to write EOS, codec tail and the trimmed granule.
/// Files are created exclusively to avoid overwriting an earlier probe.
pub struct TrackWriter {
    writer: PacketWriter<'static, BufWriter<File>>,
    encoder: OpusEncoder,
    pending: VecDeque<f32>,
    samples: u64,
    encoded: u64,
}

impl TrackWriter {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut encoder = OpusEncoder::new(SAMPLE_RATE as i32, 1, Application::Voip)
            .map_err(anyhow::Error::msg)?;
        encoder.bitrate_bps = 24_000;
        let mut writer = PacketWriter::new(BufWriter::new(file));
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 1]); // version, mono
        head.extend(((PRE_SKIP * 3) as u16).to_le_bytes());
        head.extend(SAMPLE_RATE.to_le_bytes());
        head.extend([0, 0, 0]); // output gain 0, channel mapping family 0
        writer.write_packet(head, SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        let vendor = b"notetaker";
        let mut tags = b"OpusTags".to_vec();
        tags.extend((vendor.len() as u32).to_le_bytes());
        tags.extend(vendor);
        tags.extend(0_u32.to_le_bytes());
        writer.write_packet(tags, SERIAL, PacketWriteEndInfo::EndPage, 0)?;
        Ok(Self {
            writer,
            encoder,
            pending: VecDeque::new(),
            samples: 0,
            encoded: 0,
        })
    }

    pub fn write_samples(&mut self, samples: &[f32]) -> Result<()> {
        ensure!(
            samples.iter().all(|s| s.is_finite()),
            "non-finite audio sample"
        );
        self.samples += samples.len() as u64;
        self.pending
            .extend(samples.iter().map(|s| s.clamp(-1.0, 1.0)));
        while self.pending.len() >= FRAME_SAMPLES {
            self.encode_packet(false)?;
        }
        Ok(())
    }

    /// Preserve the common timeline: fill missing packets with silence and trim
    /// overlaps when a capture clock or stream restarts. Returns the gap length.
    pub fn write_frame(&mut self, frame: &AudioFrame) -> Result<u64> {
        let start = sample_position(frame.captured_at);
        let gap = start.saturating_sub(self.samples);
        self.pad_to(start)?;
        let overlap = self.samples.saturating_sub(start) as usize;
        if overlap < frame.samples.len() {
            self.write_samples(&frame.samples[overlap..])?;
        }
        Ok(gap)
    }

    pub fn samples_written(&self) -> u64 {
        self.samples
    }

    pub fn pad_to(&mut self, samples: u64) -> Result<()> {
        let zeros = [0.0; FRAME_SAMPLES];
        while self.samples < samples {
            let count = (samples - self.samples).min(FRAME_SAMPLES as u64) as usize;
            self.write_samples(&zeros[..count])?;
        }
        Ok(())
    }

    pub fn finish_at(mut self, duration: Duration) -> Result<()> {
        let end = sample_position(duration);
        ensure!(
            self.samples <= end,
            "audio exceeds requested track duration"
        );
        self.pad_to(end)?;
        self.finish()
    }

    pub fn finish(mut self) -> Result<()> {
        // Flush the lookahead as well as any partial final frame. Padding is
        // excluded by the final granule, so even sub-frame tracks retain length.
        self.pending.extend(std::iter::repeat_n(0.0, PRE_SKIP));
        while !self.pending.is_empty() {
            let last = self.pending.len() <= FRAME_SAMPLES;
            if last {
                self.pending.resize(FRAME_SAMPLES, 0.0);
            }
            self.encode_packet(last)?;
        }
        self.writer.into_inner().flush()?;
        Ok(())
    }

    fn encode_packet(&mut self, last: bool) -> Result<()> {
        let input: Vec<f32> = self.pending.drain(..FRAME_SAMPLES).collect();
        let mut packet = vec![0; 1276];
        let bytes = self
            .encoder
            .encode(&input, FRAME_SAMPLES, &mut packet)
            .map_err(anyhow::Error::msg)?;
        packet.truncate(bytes);
        self.encoded += FRAME_SAMPLES as u64;
        let (end, granule) = if last {
            (
                PacketWriteEndInfo::EndStream,
                (self.samples + PRE_SKIP as u64) * 3,
            )
        } else {
            // Flush each 20 ms packet so a failed probe leaves recoverable pages.
            (PacketWriteEndInfo::EndPage, self.encoded * 3)
        };
        self.writer.write_packet(packet, SERIAL, end, granule)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Channel, LevelMeter, sample_offset};
    use ogg::reading::PacketReader;
    use opus_rs::OpusDecoder;

    #[test]
    fn opus_roundtrip_headers_duration_levels_and_timeline_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.opus");
        let mut writer = TrackWriter::create(&path).unwrap();
        assert!(TrackWriter::create(&path).is_err());
        let samples: Vec<f32> = (0..16_017)
            .map(|n| 0.4 * (std::f32::consts::TAU * 440.0 * n as f32 / 16_000.0).sin())
            .collect();
        let gap = 800;
        assert_eq!(
            writer
                .write_frame(&AudioFrame {
                    channel: Channel::Mic,
                    captured_at: sample_offset(gap),
                    samples,
                })
                .unwrap(),
            gap
        );
        assert_eq!(writer.samples_written(), gap + 16_017);
        writer.finish().unwrap();
        let mut reader = PacketReader::new(File::open(path).unwrap());
        let head = reader.read_packet_expected().unwrap();
        assert_eq!(&head.data[..8], b"OpusHead");
        assert_eq!(head.data.len(), 19);
        assert_eq!(
            u16::from_le_bytes(head.data[10..12].try_into().unwrap()),
            312
        );
        assert_eq!(
            &reader.read_packet_expected().unwrap().data[..8],
            b"OpusTags"
        );
        let mut decoder = OpusDecoder::new(16_000, 1).unwrap();
        let mut decoded = Vec::new();
        let mut end = 0;
        while let Some(packet) = reader.read_packet().unwrap() {
            let mut pcm = [0.0; FRAME_SAMPLES];
            let count = decoder
                .decode(&packet.data, FRAME_SAMPLES, &mut pcm)
                .unwrap();
            decoded.extend_from_slice(&pcm[..count]);
            end = packet.absgp_page();
            if packet.last_in_stream() {
                assert!(reader.read_packet().unwrap().is_none());
                break;
            }
        }
        assert_eq!(end, (gap + 16_017 + PRE_SKIP as u64) * 3);
        let pcm = &decoded[PRE_SKIP..PRE_SKIP + gap as usize + 16_017];
        let mut meter = LevelMeter::default();
        meter.push(&pcm[gap as usize + 200..pcm.len() - 200]);
        assert!((meter.levels().rms - 0.4 / 2_f64.sqrt()).abs() < 0.03);
        assert!(pcm[..600].iter().all(|s| s.abs() < 0.01));
    }

    #[test]
    fn empty_and_short_tracks_have_correct_eos_granules() {
        let dir = tempfile::tempdir().unwrap();
        for count in [0, 1, 216, 320, 640] {
            let path = dir.path().join(format!("{count}.opus"));
            let mut writer = TrackWriter::create(&path).unwrap();
            writer.write_samples(&vec![0.0; count]).unwrap();
            writer.finish().unwrap();
            let mut reader = PacketReader::new(File::open(path).unwrap());
            let mut last = None;
            while let Some(packet) = reader.read_packet().unwrap() {
                last = Some(packet);
            }
            let last = last.unwrap();
            assert!(last.last_in_stream());
            assert_eq!(last.absgp_page(), (count as u64 + PRE_SKIP as u64) * 3);
        }
    }
}
