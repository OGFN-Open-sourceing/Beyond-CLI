//! Speech-to-text via Windows SAPI (`System.Speech`), driven from PowerShell.
//!
//! EXPERIMENTAL: not yet tested with a real microphone, and the Win+H
//! fallback is unverified. Treat this module as unfinished.
//!
//! Windows ships a speech engine in .NET Framework, so no extra download or
//! API key is needed. A hidden PowerShell process listens on the default
//! microphone with a dictation grammar and prints each phrase the moment it
//! is recognized; a reader thread forwards those lines to the UI, so words
//! land in the input box while you are still talking.
//!
//! A session ends on its own after a few seconds of silence, or when the
//! caller stops it (F9 again). PCs without a SAPI recognizer for any
//! language get `Failure::NoRecognizer`, and the UI falls back to Windows'
//! own voice typing (see `win::open_voice_typing`).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// Longest a single dictation session may run.
const MAX_SECONDS: u32 = 120;

/// What a session reports back, in order: any number of phrases, then `Done`.
pub enum Event {
    Phrase(String),
    Done(Result<(), Failure>),
}

pub enum Failure {
    /// No SAPI speech recognizer is installed on this PC.
    NoRecognizer,
    Other(String),
}

/// A running dictation; dropping it stops the microphone.
pub struct Session(Arc<Mutex<Child>>);

impl Session {
    /// Stop listening. Phrases already recognized still arrive.
    pub fn stop(&self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

// Line protocol: `__P__<text>` per phrase, `__NONE__` when no recognizer is
// installed, `__ERR__<why>` on any other failure; anything else is noise.
const SCRIPT: &str = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
function Say($s) { [Console]::Out.WriteLine($s); [Console]::Out.Flush() }
try { Add-Type -AssemblyName System.Speech }
catch { Say '__ERR__speech engine unavailable'; exit 1 }
$all = @([System.Speech.Recognition.SpeechRecognitionEngine]::InstalledRecognizers())
if ($all.Count -eq 0) { Say '__NONE__'; exit 1 }
# prefer the language Windows is displayed in, then any English, then anything
$ui = [Globalization.CultureInfo]::CurrentUICulture.Name
$pick = $all | Where-Object { $_.Culture.Name -eq $ui } | Select-Object -First 1
if (-not $pick) { $pick = $all | Where-Object { $_.Culture.Name -like 'en-*' } | Select-Object -First 1 }
if (-not $pick) { $pick = $all[0] }
try {
    $e = New-Object System.Speech.Recognition.SpeechRecognitionEngine($pick)
    $e.SetInputToDefaultAudioDevice()
}
catch { Say '__ERR__no microphone found'; exit 1 }
$e.LoadGrammar((New-Object System.Speech.Recognition.DictationGrammar))
$e.EndSilenceTimeout = [TimeSpan]::FromSeconds(0.8)
$deadline = (Get-Date).AddSeconds(__SECONDS__)
$heard = $false
$quiet = 0
while ((Get-Date) -lt $deadline) {
    # the timeout only limits the wait for speech to start, not its length
    $r = $e.Recognize([TimeSpan]::FromSeconds(1))
    if ($r -and $r.Text) { Say ('__P__' + $r.Text); $heard = $true; $quiet = 0 }
    else {
        $quiet++
        # 3 s of silence after speech ends the session; 8 s if nothing was said
        if (($heard -and $quiet -ge 3) -or $quiet -ge 8) { break }
    }
}
$e.Dispose()
"#;

/// Start listening; events arrive on `tx` until `Event::Done`.
pub fn start(tx: UnboundedSender<Event>) -> Result<Session, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = SCRIPT.replace("__SECONDS__", &MAX_SECONDS.to_string());
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("failed to launch speech engine: {e}"))?;
    let stdout = child.stdout.take().ok_or("speech engine has no output")?;

    std::thread::spawn(move || {
        let mut outcome = Ok(());
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let line = line.trim();
            if let Some(text) = line.strip_prefix("__P__") {
                let _ = tx.send(Event::Phrase(text.trim().to_string()));
            } else if line == "__NONE__" {
                outcome = Err(Failure::NoRecognizer);
            } else if let Some(why) = line.strip_prefix("__ERR__") {
                outcome = Err(Failure::Other(why.trim().to_string()));
            }
        }
        // the pipe closed: the engine finished, failed, or was stopped
        let _ = tx.send(Event::Done(outcome));
    });
    Ok(Session(Arc::new(Mutex::new(child))))
}
