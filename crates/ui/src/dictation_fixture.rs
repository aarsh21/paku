//! Opt-in native keyboard fixture: real composer lifecycle, no microphone or inference.
//! Sparse model placeholders belong only in an isolated fixture data directory.
use crate::{dictation, settings};
use gpui::App;
use std::{
    cell::{Cell, RefCell},
    fs::File,
    io::Write,
    path::Path,
    rc::Rc,
    time::Instant,
};

struct Log {
    file: RefCell<File>,
    epoch: Instant,
    next_capture: Cell<u64>,
}

impl Log {
    fn record(&self, event: &str, capture: u64, finished: bool, age: u128) {
        // Metadata only: never record draft/transcript text, device names, or audio.
        let entry = serde_json::json!({
            "event": event,
            "capture": capture,
            "elapsed_ms": self.epoch.elapsed().as_millis(),
            "capture_ms": age,
            "finished": finished,
        });
        let mut file = self.file.borrow_mut();
        writeln!(file, "{entry}").expect("write dictation fixture metadata");
        file.flush().expect("flush dictation fixture metadata");
        tracing::info!(target: "paku_ui::dictation_fixture", event, capture, finished,
            capture_ms = age as u64, "Fixture transcriber lifecycle");
    }
}

struct Fake {
    log: Rc<Log>,
    capture: u64,
    started: Instant,
    listening: bool,
    finished: bool,
    final_pending: bool,
}

impl dictation::Transcriber for Fake {
    fn poll(&mut self) -> Option<dictation::Event> {
        if !self.listening {
            self.listening = true;
            self.log.record(
                "listening",
                self.capture,
                self.finished,
                self.started.elapsed().as_millis(),
            );
            return Some(dictation::Event::Listening);
        }
        if std::mem::take(&mut self.final_pending) {
            self.log.record(
                "final",
                self.capture,
                self.finished,
                self.started.elapsed().as_millis(),
            );
            return Some(dictation::Event::Final("fixture spoken text".into()));
        }
        None
    }

    fn finish(&mut self) {
        // Log every invocation, including duplicates, so evidence catches double finish.
        self.log.record(
            "finish",
            self.capture,
            self.finished,
            self.started.elapsed().as_millis(),
        );
        if !self.finished {
            self.finished = true;
            self.final_pending = true;
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.log.record(
            "drop",
            self.capture,
            self.finished,
            self.started.elapsed().as_millis(),
        );
    }
}

/// Enable fake dictation after settings initialization. `data` MUST be isolated
/// fixture storage: its model files are sparse placeholders, not usable weights.
/// `log` is a JSONL file, truncated for this run. No input devices are enumerated,
/// model downloaded/loaded, or native capture session constructed.
pub fn configure(data: &Path, log: &Path, cx: &mut App) -> anyhow::Result<()> {
    let model = data.join("models/parakeet-tdt-0.6b-v3-int8");
    let marker = model.join(".paku-fake-transcriber-fixture");
    anyhow::ensure!(
        !model.exists() || marker.is_file(),
        "refusing to overwrite a non-fixture model directory"
    );
    std::fs::create_dir_all(&model)?;
    std::fs::write(&marker, "sparse placeholders; fake transcriber only\n")?;
    let manifest = paku_voice::manifest();
    for file in manifest.files {
        File::create(model.join(file.name))?.set_len(file.size)?;
    }
    std::fs::write(model.join("verified"), manifest.revision)?;
    anyhow::ensure!(
        paku_voice::installed(&model),
        "fixture model readiness failed"
    );

    let log = Rc::new(Log {
        file: RefCell::new(File::create(log)?),
        epoch: Instant::now(),
        next_capture: Cell::new(0),
    });
    cx.set_global(dictation::TestTranscriberFactory(Rc::new(move || {
        let capture = log.next_capture.get() + 1;
        log.next_capture.set(capture);
        log.record("start", capture, false, 0);
        Box::new(Fake {
            log: log.clone(),
            capture,
            started: Instant::now(),
            listening: false,
            finished: false,
            final_pending: false,
        })
    })));
    dictation::init(data.to_path_buf(), cx);
    settings::update(settings::SavePolicy::Immediate, cx, |s| {
        s.dictation_enabled = true
    });
    Ok(())
}
