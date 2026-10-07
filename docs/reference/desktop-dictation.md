# Desktop dictation

Settings → Voice downloads NVIDIA Parakeet TDT 0.6B v3 for this device and enables the composer microphone. The optional INT8 ONNX download is **670,479,942 bytes**, derived from the pinned manifest, and is independent of the chat's engine host. Turn off hides dictation without deleting the model; Remove model disables it and removes the local cache.

Dictation is hold to talk: hold the microphone (or Enter/Space while it is focused) or the **Hold to dictate** shortcut, rebindable in Settings → Voice (default Cmd/Ctrl+D), and release to transcribe. Releasing outside the microphone still counts; for the shortcut, letting go of its key or any of its modifiers ends the hold, since macOS does not deliver key-up for Command chords. On Linux, a modified shortcut's key-up waits up to **50 ms** for a matching repeat press, because some native input paths send release/press pairs during auto-repeat. A repeat keeps the original capture alive; releasing a required modifier or leaving focus still ends it immediately. With the modifier still held, a real key release finalizes after that small grace period. Repressing the same chord within that period merges into the hold. The observed release time—not delayed delivery—is used for tap classification. This applies only to the dictation shortcut, not ordinary typing or mouse holds. A release within 300 ms is treated as a tap and explains hold to talk instead of transcribing. Assistive-technology Click cannot hold, so it starts and then finishes. Recording is limited to one minute. Release transcribes locally into the selected draft range. It never invokes an agent or submits on its own. Explicit Send waits for finalization, then follows the normal Send/Queue path once. If no speech is recognized, the composer explains this and preserves the draft without sending. Esc cancels; cancelling or a failed/timed-out finalization never sends. Microphone permission is requested on first use; if the permission dialog takes focus, retry after granting permission.

The model detects the spoken language automatically and does not translate. Supported languages: Bulgarian, Croatian, Czech, Danish, Dutch, English, Estonian, Finnish, French, German, Greek, Hungarian, Italian, Latvian, Lithuanian, Maltese, Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Spanish, Swedish, Ukrainian. Technical names may need correction, especially in mixed-language speech.

No audio files are created by production dictation. Audio stays in a bounded, transient buffer on the desktop running the UI; it is not attached, logged, synchronized or sent to the coding-agent host. Dictated text is an ordinary editable draft and is handled like typed text. Model downloads contact Hugging Face; transcription does not use the network.

## Implementation and limits

`paku-voice` owns downloads, model integrity, CPAL capture, anti-aliased conversion of device-rate audio to 16 kHz, a capture thread, and a single background Parakeet worker. `paku-ui::dictation` owns permissions, local settings and the small transcription event interface. Composer inputs own draft-range/generation protection and grouped undo. A manual edit, selection move, IME update, focus leaving the composer, chat switch or queue-draft replacement invalidates the session. Attachments retain their existing behavior.

Capture starts as soon as permission and the audio device allow, independently of background model verification/loading. “Listening” is emitted only after the microphone stream starts; there is no hover prewarming. Stop closes capture and retains the bounded audio buffer until the model is ready, within the existing finalization deadline. A load failure closes capture before reporting failure. The production adapter performs offline final transcription on Stop. No live partials or native streaming are advertised. Editor partial-result behavior is tested through a deterministic adapter. Audio is capped at 60 seconds; UI finalization has a 30-second deadline. An in-flight native ONNX call cannot be forcibly interrupted safely: cancellation signals capture to stop independently of model loading (the capture loop checks every 10 ms) and invalidates results; one worker keeps native work bounded and rejects overlapping sessions until it returns. Cached weights unload after 30 idle seconds or model removal. Settings refuses removal while capture, loading, or inference is still active; after cancelling uninterruptible native work, retry removal once it returns.

Pinned runtime: `parakeet-rs = 0.3.8`, `ort/ort-sys = 2.0.0-rc.13`, ONNX Runtime **1.28.0**, CPU execution; `cpal = 0.17.3`, `rubato = 0.16.2`. The download manifest and conversion attribution are in `crates/voice/model.json` and `crates/voice/NOTICE.md`. The conversion pins immutable bytes but its publisher does not provide the source-weight revision or exact INT8 conversion tool version; this provenance limitation is explicit.

Release targets are macOS arm64, Linux x86_64/aarch64 and Windows x86_64. Linux CI installs ALSA headers. macOS uses AVFoundation only for permission, and its bundle includes microphone usage text and the hardened-runtime audio-input entitlement. The selected ONNX binary distribution has no Intel macOS build; Intel development requires an independently built matching ONNX Runtime. Windows' prebuilt ONNX archive links the system DirectML/DX12 libraries even though the chosen execution provider is CPU. Real-model inference, desktop UI, packaging and physical-microphone validation are separate checks; the PR records the platform evidence. iOS does not depend on this crate.

On Windows, microphone access for desktop apps must be enabled in the system privacy settings. On Linux, the selected ALSA input must be available to the user and the audio server. Settings → Voice lists available inputs and refreshes while visible; a disconnected selection falls back to the system default. Test built-in, USB and Bluetooth microphones on the target OS: a synthetic WAV test cannot establish permission, device routing or capture behavior.

Linux releases dynamically link the system ALSA runtime (`libasound.so.2`). It is required even with dictation disabled or in headless mode; no audio device or microphone permission is needed to run the engine. Both Linux installers check executable startup before activating an installation. The updater also checks a staged binary before replacing the active version.

## Verification

Deterministic editor fixtures cover partial replacement, Unicode, undo/redo, failure, stale events, cancellation, Send finalization, queue editing, navigation, focus and input-request takeover. `cargo test -p paku-voice` covers pinned manifest shape/size, cancellation, corrupt cache rejection, and deterministic capture/load barriers for Stop, cancellation, failure, device startup, recording limits, and busy admission. The `verify` example runs explicitly supplied synthetic/public PCM16 WAV files and prints transcripts/timings for development only; production never prints transcripts.

For native startup timing, launch a development bundle with `RUST_LOG=info,paku_ui::dictation=debug`. The `activation_to_listening_ms` field measures from composer dictation activation to the UI entering Listening, including permission waiting and the 40 ms event polling cadence. It contains no audio or transcript; display presentation may follow on the next frame. Measure cold and cached model runs separately.

### Keyboard-hold diagnostics

`paku_ui::dictation` records shortcut press/release lifecycle at info level. A release includes `reason` (`key-up`, `modifier-up`, or `editor-blur`) and `held_ms`; blur also records whether the editor's focus handle remains focused. These records contain no audio, draft, transcript, device name, or unmodified typing. They distinguish a prematurely released keyboard hold from microphone/transcription failures.

The opt-in native fixture uses the actual composer (or the whole shell), private storage, sparse placeholder model files, and a **fake transcriber**. It never accesses a microphone or loads/downloads model weights:

```sh
cargo build --locked -p paku-ui --example dictation-fixture --features dictation-fixture
PAKU_DICTATION_FIXTURE_SHELL=1 PAKU_DICTATION_FIXTURE_ZOOM=1.25 \
  node scripts/test-paku-dictation-input.mjs /tmp/paku-dictation-native
```

Set `PAKU_DICTATION_REPLAY_RELEASE_PAIRS=1` to replay the diagnosed repeat sequence (first release after 250 ms, then 60 release/press pairs). The old build restarted fake capture 61 times; the corrected build keeps one capture and finalizes once after release. These are private-display fake-transcriber checks, not microphone accuracy results.

The runner requires Xvfb, Weston, xdotool and ImageMagick; it starts private displays only. `PAKU_WESTON_MODULE_ROOT` and `PAKU_WESTON_SHELL` can point to an extracted Weston runtime. It asserts one capture across native keyboard repeat/voice animation, no finish/drop while held, and exactly one finalization after release. Set `PAKU_DICTATION_NATIVE_WAYLAND=0` for a separate private X11 backend test—not a user-desktop workaround. Fake-transcriber success does not certify real microphone capture or reproduce every compositor/input configuration.

Real-model verification is an explicit development check using the `verify` example and external synthetic/public WAV files. Routine CI does not download the model or retain speech fixtures in the repository.

Future mobile implementation is scoped in the [PR #591 handoff](https://github.com/zeronsh/zeron/pull/591#issuecomment-5869764738), including native capture, lifecycle cancellation, mobile inference benchmarks, UTF-16/UTF-8 selection handling and preservation of iOS delivery modes.

See the task evidence under `/Users/gaelcado/paku/evidence/voice/` for actual runtime transcripts, hardware measurements and native build captures. Standalone synthetic model accuracy is distinct from a live microphone test.
