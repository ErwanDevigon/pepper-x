# Pepper X

Local dictation for GNOME on Linux. Hold a shortcut, speak, release. Or load a recording you already made. The text lands in the focused app. Audio and models stay on the machine.

Pepper X started from a strong base: a GNOME app, a local ASR engine, and a small cleanup model. This tree keeps that shape. The speed that matters is keeping the selected models loaded for the life of the process. A fresh dictation then reuses them instead of loading them again.

## Recommended setup

1. **ASR:** `fluid-speech-pro` or `parakeet-ultra`. Both are Parakeet TDT models in ONNX. They run as one pass on the finished recording. Leave **Use GPU for transcription** off. On a cached model the CPU pass is already very short, and this build does not keep those weights on the GPU yet. Turning the switch on asks ONNX Runtime for CUDA, then falls back to CPU when that session does not load. It does not reserve a multi-gigabyte VRAM block.
2. **Cleanup:** `Qwen3.5-4B-Q4_K_M`, with **Use GPU for cleanup** on and the GPU layers set (100 on a 12 GB card is the setup measured below). This is the real GPU win. 2B is lighter. 9B is slower and not worth it for short dictation. 0.8B is the small CPU-era default and is easy to outgrow.
3. Leave the TDT model selected. The app loads it at startup and keeps the session for the life of the process. The next dictation reuses it. That cache, not a CUDA provider, is what makes transcription feel immediate.
4. Quit from the panel menu (**Quit Pepper X**) or with `gapplication action com.obra.PepperX quit`. Warning: Do not suspend the process with Ctrl-Z: it holds an AT-SPI keyboard grab and the session input freezes until the process exits cleanly.

Nemotron remains available. It streams audio while you hold the key. The overlay still waits until release, then cleanup, before it inserts text. On the machine below, a cached TDT model felt faster than that streaming path, and the transcript quality was better. Fluid Speech Pro and Parakeet Ultra choose the language from the audio and hide that control. Prefer Nemotron only if you need its multilingual language picker.

A fresh settings file leaves transcription on CPU and cleanup on the GPU. If **Use GPU for transcription** is on and the CUDA session does not load, the app uses CPU anyway.

## What it does

- **Hold Alt+Super** (configurable) to record. Release to stop. There is also a toggle shortcut.
- **Transcription** after release for Fluid Speech Pro and Parakeet Ultra. Nemotron streams during the hold, then flushes on release.
- **Cleanup** with Qwen 3.5: fillers, punctuation, capitalization, self-corrections. The prompt closes the Qwen think block so the model does not answer with `Thinking Process:`.
- **Insertion** into the focused widget: AT-SPI first, then a clipboard paste, then an XKB-aware uinput helper for apps that ignore accessibility.
- **Widget context** for cleanup: the accessible text around the caret, not a screenshot.
- **Optional speaker filter** (experimental).
- **Audio files** from disk. The upstream app is built around the live shortcut. This tree also transcribes a file you bring from a dictaphone or another recorder, including long notes, then runs the same cleanup.

## Audio files

Open **History** and choose **Load audio file**. The picker accepts WAV, MP3, FLAC, and Ogg. The run shows up under **Audio files**, separate from live **Recordings**. The app writes a Markdown transcript next to the audio file: `notes.wav` becomes `notes.md` in the same folder. The file has the source path, the model, the raw transcript, and the cleaned text when cleanup is on.

The same transcription exists without the window. That command prints the text and does not write the Markdown file:

```sh
pepper-x --transcribe-wav-and-cleanup recording.wav
```

Fluid Speech Pro and Parakeet Ultra transcribe the whole file in one pass, then Qwen cleans it. The useful case for a long dictaphone note is the cached session plus GPU cleanup: you get a clean transcript without sitting through the recording again.

## Performance

Older notes in this file described Nemotron on CPU, for example `transcribe=0.0s` because streaming overlapped the recording, then about half a second of cleanup. That no longer describes the recommended path.

These figures are from one RTX 3060. Qwen 3.5 4B Q4_K_M had 100 GPU layers. The TDT model was already cached in RAM. The table is the cleanup step only, logged after the transcript exists. It is not a benchmark suite.

| Dictation length | Cleanup on GPU | Notes |
| --- | --- | --- |
| 18–35 characters | 140–210 ms | Fluid Speech Pro already cached in RAM |
| ~60–80 characters | 230–360 ms | Fluid or Parakeet Ultra |
| ~120–150 characters | 460–590 ms | Fluid Speech Pro |
| 243 characters | 822 ms | Parakeet Ultra |
| 455 characters | 1.5 s | Parakeet Ultra |

The same 4B model on CPU, for a 29-character line, took about 3.8 s, of which about 2.1 s was decode. On GPU the decode of a short answer was about 12–27 ms. That is why cleanup stays on the GPU by default.

The cleanup prompt prefill is about 230–250 ms at the start of a hold, on a model that is already resident (about 3.3 GiB of VRAM for the 4B). Short answers often change the text by only a character or two.

Memory, same session:

- **Qwen 3.5 4B** stays on the GPU when **Use GPU for cleanup** is on. `nvidia-smi` shows it on `pepperx-cleanup-helper`, about 3.3 GiB. That block is the expensive one, and it is the one that pays for itself.
- **Fluid Speech Pro** stays in the `pepper-x` process, about 2.6 GiB of RAM. With the transcription switch off, that process does not hold the model on the GPU. Parakeet Ultra behaves the same way. A log line that says `cuda` only means the app asked for the CUDA provider. Check `nvidia-smi` for a real allocation.
- The first load logs `ASR model cached`. Later holds log `ASR reusing cached model`. The provider in parentheses is `cpu` or `cuda`.

## Models

Launch the app, open **Models**, and download anything missing. The catalog ids are:

| Role | Id | Notes |
| --- | --- | --- |
| ASR | `fluid-speech-pro` | TDT ONNX, ~2.6 GB encoder, batch, CPU by default |
| ASR | `parakeet-ultra` | TDT ONNX, ~2.4 GB, same runtime path as Fluid |
| ASR | Nemotron 0.6B int8 | ~850 MB, streaming, CPU in this build |
| ASR | Nemotron 3.5 multilingual int8 | Streaming, language dropdown (`fr-FR` default, or `auto`) |
| Cleanup | `qwen3.5-4b-q4_k_m.gguf` | Recommended, GPU on by default |
| Cleanup | `qwen3.5-2b-q4_k_m.gguf` | ~1.3 GB, faster, lighter |
| Cleanup | `qwen3.5-9b-q4_k_m.gguf` | ~5.3 GB, slower on short dictation |
| Cleanup | `qwen3.5-0.8b-q4_k_m.gguf` | ~500 MB |

## Install

Ubuntu 25.04+ or Fedora 42+. GNOME 48+ on Wayland. Rust 1.92 or newer (`gtk4-rs` 0.11). Distro `cargo` is often older; use [rustup](https://rustup.rs) if `rustc --version` is below 1.92.

```sh
# Ubuntu
sudo apt install \
  build-essential cmake clang libclang-dev \
  libadwaita-1-dev libatspi2.0-dev \
  libglib2.0-dev libgtk-4-dev \
  libpipewire-0.3-dev libxkbcommon-dev \
  pkg-config \
  wl-clipboard xclip

# Fedora
sudo dnf install \
  cmake gcc gcc-c++ clang clang-devel \
  at-spi2-core-devel glib2-devel \
  gtk4-devel libadwaita-devel libxkbcommon-devel pipewire-devel \
  pkgconf-pkg-config \
  wl-clipboard xclip
```

`libclang` is required by `llama-cpp-sys-4`. `wl-clipboard` or `xclip` lets the uinput helper paste characters the current layout cannot type.

Your user must be in the `input` group, and uinput needs a udev rule. Log out after the group change.

```sh
sudo usermod -aG input $USER
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee /etc/udev/rules.d/99-pepper-x-uinput.rules
echo 'SUBSYSTEM=="input", ATTRS{name}=="Pepper X virtual keyboard", ENV{ID_INPUT_KEYBOARD}="1"' | sudo tee /etc/udev/rules.d/99-pepper-x-keyboard.rules
sudo udevadm control --reload-rules
```

### GPU build

Cleanup uses llama.cpp CUDA in `pepperx-cleanup-helper`. ASR CUDA uses the ONNX Runtime CUDA provider inside `pepper-x` (`parakeet-rs` feature `cuda`). Both are on in the default `Cargo.toml`. You need a working `nvidia-smi` and, for the cleanup helper, a CUDA toolkit (`nvcc`, `libcudart`). If the toolkit is not in `/usr/local/cuda`:

```sh
CUDA_HOME=/opt/cuda cargo build --release
```

CPU-only cleanup: remove `"cuda"` from `llama-cpp-4` and `llama-cpp-sys-4` in `crates/pepperx-cleanup-helper/Cargo.toml`. CPU-only ASR: remove `"cuda"` from `parakeet-rs` in `crates/pepperx-asr/Cargo.toml`. Vulkan for AMD or Intel cleanup is not on by default; see the comment in the cleanup-helper manifest.

### Build and install

```sh
cargo build --release
sudo install -m 755 target/release/pepper-x /usr/local/bin/
sudo mkdir -p /usr/libexec/pepper-x
sudo install -m 755 target/release/pepperx-uinput-helper /usr/libexec/pepper-x/
sudo install -m 755 target/release/pepperx-cleanup-helper /usr/libexec/pepper-x/
bash scripts/dev-install-extension.sh
```

The app also looks for the helpers next to the `pepper-x` binary, so a copy under `~/.local/bin` works without root if that directory is on `PATH`.

Log out and back in once so GNOME Shell loads the extension. Shell keeps extension JavaScript in memory; `disable` / `enable` does not always reload it. The panel menu is **Open Pepper X Settings**, **Open Pepper X History**, and **Quit Pepper X**.

## Usage

```sh
pepper-x
```

On startup the app starts the extension, prewarms the Qwen helper, and preloads the selected TDT model when that directory is a Parakeet layout. Default shortcut: hold Alt+Super.

Only errors go to the terminal unless you ask for more:

```sh
PEPPERX_VERBOSE=1 pepper-x
```

That adds a per-dictation `perf:` line, streaming ASR timing, cleanup prefill, and uinput layout detection. The CUDA lines (`ASR execution provider`, `ASR reusing cached model`) are printed either way.

### Text insertion

1. **AT-SPI**, when the focused widget exposes editable text.
2. **Clipboard paste**, when a clipboard tool exists.
3. **`pepperx-uinput-helper`**, for terminals, Wine, and other apps that ignore accessibility.

The helper reads the active XKB layout (not a hard-coded US map): `PEPPERX_XKB_LAYOUT` / `PEPPERX_XKB_VARIANT` if set, otherwise GNOME `mru-sources`, then `setxkbmap -query`, then `/etc/default/keyboard`. It types chords, including dead keys, when every character exists on that layout. If any character does not, it pastes the whole string with `wl-copy`, `xclip`, or `xsel`, and restores the previous clipboard when it can. Some terminals want Ctrl+Shift+V and will ignore Ctrl+V.

### Settings

- **Recording** — hold shortcut, toggle shortcut, microphone, sounds, speaker filter, test dictation.
- **Cleanup** — on/off, widget context, prompt profile, custom prompt. **Enable cleanup** stays on this page.
- **Corrections** — preferred spellings and mishear replacements.
- **Models** — **Use GPU for transcription** (off by default), transcription model, language dropdown for multilingual Nemotron only, cleanup model, **Use GPU for cleanup** (on by default), then GPU layers. There is no layer count for the transcription model.
- **History** — replay a recording with other models, edit the prompt, diff, playback.
- **General** — launch at login.
- **Diagnostics** — runtime status.

### CLI

```sh
pepper-x --transcribe-wav recording.wav
pepper-x --transcribe-wav-and-cleanup recording.wav
pepper-x --rerun-archived-run <run-id>
```

## Architecture

- **`pepper-x`** — GTK4 / libadwaita. Recording, settings, history, and the ONNX ASR session.
- **`pepperx-cleanup-helper`** — long-lived llama.cpp process for Qwen. It is separate so ONNX Runtime and llama.cpp do not share one process.
- **`pepperx-uinput-helper`** — long-lived virtual keyboard.
- **`pepperx@obra`** — GNOME Shell extension: panel menu, status pill, D-Bus.

| Crate | Role |
| --- | --- |
| `pepperx-asr` | Nemotron streaming, Parakeet TDT batch, CUDA session cache |
| `pepperx-cleanup` | Prompt assembly and the helper protocol |
| `pepperx-cleanup-helper` | Qwen inference |
| `pepperx-audio` | PipeWire capture |
| `pepperx-corrections` | Replacement lists |
| `pepperx-models` | Catalog, download, readiness |
| `pepperx-platform-gnome` | evdev shortcut, AT-SPI insert and context |
| `pepperx-ipc` | D-Bus for the extension |
| `pepperx-uinput-helper` | Layout-aware typing and clipboard fallback |

## Tests

```sh
cargo test --workspace
```

## License

See the license files in each crate.
