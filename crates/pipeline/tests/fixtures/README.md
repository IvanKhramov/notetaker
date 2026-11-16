Synthetic 440 Hz tone, 48 kHz stereo, 0.24 seconds, generated locally with ffmpeg:

```sh
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.24 -ac 2 -c:a <codec> tone.<ext>
```

Codecs: mp3/libmp3lame, m4a and mp4/aac, ogg/libvorbis, flac/flac,
webm and mkv/libopus. Tests read these small fixtures with Rust decoders only;
ffmpeg is not required to run the test suite.

`voice-learning.mp4`: 12 s of the same synthetic 440 Hz stereo tone, AAC at
24 kbit/s in MP4 (`-movflags +faststart`). Used for native-only historical voice
learning and temporary-file cleanup tests. No speech or real voices.
