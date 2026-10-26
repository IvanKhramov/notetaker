# Windows audio capture probe

`notetaker probe` diagnoses two separate tracks: microphone and system audio.
It does not transcribe or upload audio. Live capture is Windows-only; `FileSource`
is available for pipeline tests on other operating systems.

## Get the executable

- In GitHub Actions, open a successful **CI** run for this branch, download
  **notetaker-windows-x64** under Artifacts, and extract `notetaker.exe`.
- Or install stable Rust with the MSVC toolchain and Visual Studio C++ Build Tools,
  then run from the repository in PowerShell:

  ```powershell
  cargo build --release -p notetaker-cli -j 2
  Copy-Item .\target\release\notetaker.exe .\notetaker.exe
  ```

No libopus, DLL download or codec installation is required. Tracks use Ogg Opus
(`.opus`, mono, 16 kHz input, 24 kbit/s) through the pinned pure-Rust `opus-rs`
encoder and `ogg` muxer. Ogg Opus granules use 48 kHz units, as required by the
container; a player may therefore display 48 kHz even though capture output is
16 kHz. The encoder delay is removed by the Opus pre-skip field.

## Prepare Windows

1. Check `winver`. Endpoint loopback works on Windows 10/11. Application loopback
   requires **build 20348 or newer** (including Windows 11). Windows 10 2004/build
   19041 does **not** meet that requirement; activation failure should fall back
   to endpoint capture and appear in the report.
2. In Settings > Privacy & security > Microphone (Windows 11), enable microphone
   access and **Let desktop apps access your microphone**. Choose the intended
   default input and output devices in Sound settings. Check the call's own
   device selection; it must use the output endpoint being monitored.
3. Join a test call and have the remote participant speak while the probe runs.
   Speak locally as well. Start with headphones. Use a fresh output directory
   for every run; the probe refuses to overwrite existing files.

## Run the scenarios

The default run lasts 10 seconds and captures the full default output endpoint:

```powershell
.\notetaker.exe probe --out .\probe-endpoint
```

Every run prints capture/render device IDs, their default status, and audio
sessions with PID, executable name (when accessible), and active/inactive status.
For a longer process test, choose the parent application by exact executable name
(case insensitive), or choose its PID in Task Manager > Details:

```powershell
.\notetaker.exe probe --seconds 25 --app Zoom.exe --out .\probe-zoom-headphones
.\notetaker.exe probe --seconds 25 --app ms-teams.exe --out .\probe-teams-headphones
.\notetaker.exe probe --seconds 25 --app msedge.exe --out .\probe-browser-headphones
.\notetaker.exe probe --seconds 25 --pid 1234 --out .\probe-selected-pid
```

- **Zoom:** use an actual meeting with audible remote speech. Confirm your voice
  is in `mic.opus` and the remote voice is in `system.opus`.
- **New Teams/WebView2:** use `ms-teams.exe`, not the legacy `Teams.exe` unless
  that is what is running. The application and its descendants are included.
  Teams audio may still evade process loopback (OpenWhispr issue #1265).
  Keep remote speech playing past 10 seconds; confirm fallback if process audio
  stays silent while the default endpoint is audible. Run the endpoint baseline
  as well. A successful process capture does not need to fall back.
- **Browser call:** join a WebRTC meeting in Edge/Chrome/Firefox and use the
  browser executable (`msedge.exe`, `chrome.exe`, `firefox.exe`). This includes
  other tabs in that process tree. If more than one independent root matches,
  the CLI asks for an explicit `--pid`; it does not guess.

Repeat all three scenarios **without headphones**, using speakers. AEC is
**disabled** in this milestone: remote speech may leak into the microphone track.
Record whether that happens; echo removal/quality has not been validated.

To choose a particular microphone or shorten the fallback wait:

```powershell
.\notetaker.exe probe --seconds 20 --pid 1234 --silence-seconds 3 --mic-id '<device-id>' --out .\probe-custom
```

## Check fallback and failure behavior

- With process capture selected, endpoint samples are monitored concurrently.
  After 10 continuous seconds without process audio above **-60 dBFS RMS**,
  fallback happens only on a tick where endpoint audio is above that threshold.
  No process packets counts as silence. Both streams being silent does not
  trigger fallback; any process speech resets the timer.
- A process activation/initialization error (including an unsupported Windows
  build) switches immediately. A later process read error also switches.
  Selection stays on the endpoint for the rest of the run. Console output and
  `events` in JSON include the reason and elapsed time; `fallback_used` is true.
- Endpoint capture includes **all sound on the default output device** after
  fallback. Stop unrelated players if they would confuse the test.
- As a deterministic fallback test, select a running, quiet application PID
  (for example Notepad), play speech through the default output, and run for
  20 seconds. Expect `process_silent` around 10 seconds. Repeat with the whole
  system silent and expect no silence-triggered fallback.
- During another run, switch the default microphone or output device, or unplug
  the active device. Within approximately one second the affected source should
  report an error, with no panic. This milestone stops that source; it does not
  reopen the stream. Restart the probe after changing devices.

## Inspect and send results

Each output directory contains `mic.opus`, `system.opus`, and `report.json`.
Play **both files** in VLC or another Ogg Opus player. Check local/remote voices,
truncation, distortion and missing speech before and after any fallback. Both
tracks should have the requested duration; missing packets are padded with
silence on their shared timeline, not concatenated away.

Send back:

- `report.json` from each scenario, Windows build, call app/version, selected
  devices, headphones/speakers, and the exact command.
- Whether **both files were played**, which voices were audible on each, and
  whether fallback happened while the remote participant was speaking.
- Whether a device switch produced an error and whether the process exited
  cleanly. Exit code 1 indicates capture errors; the report and finalized audio
  are still saved when capture itself fails.

The report includes inventory, process names/PIDs, events, error text, captured
and padded sample counts, and each track's overall RMS average and sample peak
in dBFS. Levels include inserted silence; digital silence is reported as
**-120 dBFS** (a finite floor for JSON). These are sample peaks, not true peaks.
Audio files are optional to share; confirm the participants agree before sending
recordings. Review the process/device inventory if you need to redact it.

Linux checks and Windows cross-compilation cannot verify driver behavior,
WebView2 process routing, device changes, audible quality or acoustic echo.
A green CI build is necessary but does not replace these listening tests.

API references: [wasapi 0.26.0](https://docs.rs/wasapi/0.26.0/wasapi/),
[process-loopback requirements](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_process_loopback_params),
[Opus encoder](https://github.com/restsend/opus-rs),
[Ogg Opus mapping](https://www.rfc-editor.org/rfc/rfc7845).
