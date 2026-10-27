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

## Kontur Talk and Yandex Telemost

For the desktop apps, check **Task Manager > Details** and note the exact exe
names for Контур.Толк and Яндекс Телемост. The built-in Telemost/Kontur aliases
are best-effort guesses, not confirmed executable names. Add the actual names in
**Настройки > Свои приложения для звонков**, one exe per line, without paths.
They supplement the built-in list and work when
**Предлагать для любых приложений с микрофоном** is disabled (it defaults to on).
System processes, notetaker itself and Windows Sound Recorder are excluded.

Test both desktop apps and browser calls on ktalk.ru, telemost.yandex.ru and
telemost.360.yandex.ru. After the start delay, check the service/browser name in
the prompt and the suggested meeting title. Window titles are best-effort hints:
only visible top-level windows in the microphone owner's process tree are read;
an inactive call tab may not be identifiable. Multiple different call windows
suppress the ambiguous title. A manually entered title takes precedence.

Browser process loopback captures **all tabs in the selected browser process
tree**, not only the detected meeting. Endpoint fallback captures all sound on
the selected output device. Confirm remote speech in both modes.

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

## Watch for calls (M7)

`notetaker watch` polls once per second and prints `CallStarted`/`CallEnded`
with the application name and PID. It only offers **«Записать?»**; it does not
start recording. Stop the watcher with Ctrl+C.

```powershell
.\notetaker.exe watch
.\notetaker.exe watch --browsers
.\notetaker.exe watch --start-seconds 5 --end-seconds 20 --dismiss-minutes 10
.\notetaker.exe watch --app custom-call.exe
```

Check these scenarios on Windows with the same interactive user as the call:

- **Zoom:** join with the microphone enabled. After at least 5 seconds of
  continuous microphone use, expect one `CallStarted` for Zoom. Leave and expect
  `CallEnded` after at least 20 seconds without microphone use.
- **Teams:** repeat with classic `Teams.exe` and new `ms-teams.exe` where available.
  New Teams is matched by its package family as well as the executable path;
  confirm that WebView2 audio does not cause duplicate hints.
- **Browser call:** run `watch --browsers`, join Meet or web Teams in Chrome,
  Edge, Firefox or Yandex, allow microphone access, and check the same 5/20-second
  transitions. Browser audio subprocesses should produce one application hint.
  Muting inside the call may keep the microphone open; leaving is the decisive
  release test.
- **Idle browser:** with `--browsers`, leave a browser open without any tab using
  the microphone for over 5 seconds. Expect no `CallStarted`. Playing music or
  video without microphone access must not trigger a hint either.
- **Browser flag:** without `--browsers`, even a browser holding the microphone
  must not trigger a hint. Opting in also detects dictation/voice messages;
  microphone use alone cannot prove that a browser tab is a meeting.
- **Debounce:** access the microphone for less than 5 seconds, release it, then
  access it again. The first short burst must not count toward the next start.
  Release for less than 20 seconds during a detected call and resume: no end
  event and no duplicate start should appear.

Defaults: start debounce **5 s**, end debounce **20 s**, dismissal cooldown
**10 min**, browser detection **off**. The library's `dismiss(pid)` suppresses
repeated proposals until the cooldown expires, including another call in that
same process; `watch` only prints events and has no accept/dismiss UI.
Additional executable rules can be supplied by application settings through
`DetectConfig.apps` (or `watch --app`); later rules override earlier ones.

The read-only Windows snapshot reads
`HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone`:
`NonPackaged` children encode executable paths with `#` in place of `\`, while
other children identify package families. Packaged descendants are also scanned.
`LastUsedTimeStart > LastUsedTimeStop` means currently using the microphone
(usually stop is zero). Both values must be present and well formed. This is
an observed Windows implementation detail, as used in
[litra-autotoggle's registry reader](https://github.com/timrogers/litra-autotoggle/blob/main/src/main.rs)
and [BreakTime's microphone detector](https://github.com/jaimaharaj1/BreakTime).
It is not a documented Windows call-status API. Exact paths/package identities
are joined to running processes; active capture sessions disambiguate owners
when possible. Ambiguous owners without session evidence are skipped. Active
**output** sessions from capture inventory are supporting evidence in `source`,
never a trigger by themselves. Registry/inventory failures print warnings and
must not produce an artificial end event. Validate registry behavior on the
Windows versions being shipped; stale records or a call app retaining the
microphone after leaving can delay or prevent `CallEnded`.

Recording separately defaults to an automatic stop after **15 minutes** without
sound above `RecordingConfig.chunks.silence_level` on either track (default
linear RMS **0.01**, **-40 dBFS**). Missing audio packets count as silence; any
sound on either track resets the wall-clock timer. For a shorter manual test:

```powershell
.\notetaker.exe record --library .\library --title 'Silence test' --auto-stop-silence-seconds 10
```

Keep both inputs quiet. Expect the auto-stop message, then final transcription
and normal completion with both audio tracks saved. Repeat with speech or sound
on either track to confirm the timer resets. Use
`--auto-stop-silence-seconds 0` to disable it. The API option is
`RecordingConfig.auto_stop_after_silence: Option<Duration>` (`None` disables),
and `PipelineEvent::AutoStopped` precedes the final pass and `Finished`.
`CallEnded` does not stop the pipeline; the application decides that policy.

Linux fake-snapshot/fake-source tests and the MSVC detector check do not validate
real ConsentStore updates, package/PID permissions, audio-session ownership,
Zoom/Teams/browser behavior, or audible thresholds on actual Windows hardware.
