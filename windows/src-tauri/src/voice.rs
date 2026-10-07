#[cfg(windows)]
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::{log, platform};

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

#[derive(Default)]
pub struct SttProcess {
    child: Mutex<Option<Child>>,
    stopping: AtomicBool,
    #[cfg(windows)]
    job: Mutex<Option<OwnedHandle>>,
}

#[derive(Default)]
pub struct TtsProcess {
    child: Mutex<Option<Child>>,
}

impl TtsProcess {
    pub fn stop(&self) -> Result<(), String> {
        let mut child_guard = self.child.lock().unwrap();
        if let Some(child) = child_guard.as_mut() {
            if child
                .try_wait()
                .map_err(|error| format!("could not check SAPI process: {error}"))?
                .is_none()
            {
                if let Err(error) = child.kill() {
                    if child
                        .try_wait()
                        .map_err(|check| format!("could not check SAPI process: {check}"))?
                        .is_none()
                    {
                        return Err(format!("could not stop SAPI: {error}"));
                    }
                }
                child
                    .wait()
                    .map_err(|error| format!("could not reap SAPI: {error}"))?;
            }
        }
        *child_guard = None;
        Ok(())
    }
}

#[tauri::command]
pub fn tts_speak(
    process: tauri::State<'_, TtsProcess>,
    text: String,
    rate: f64,
    volume: f64,
) -> Result<(), String> {
    if !rate.is_finite() || !(0.8..=1.4).contains(&rate) {
        return Err("Speech rate must be between 0.8 and 1.4.".into());
    }
    if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
        return Err("Speech volume must be between 0 and 1.".into());
    }
    process.stop()?;

    #[cfg(windows)]
    {
        let sapi_rate = ((rate - 1.0) * 25.0).round() as i32;
        let sapi_volume = (volume * 100.0).round() as u8;
        let script = format!(
            "$ErrorActionPreference = 'Stop'; \
             Add-Type -AssemblyName System.Speech; \
             [Console]::InputEncoding = New-Object System.Text.UTF8Encoding; \
             $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
             $voices = @($s.GetInstalledVoices() | \
               Where-Object {{ $_.Enabled -and $_.VoiceInfo.Culture.Name -match '^en(-|$)' }} | \
               Sort-Object {{ \
                 if ($_.VoiceInfo.Culture.Name -eq 'en-US') {{ 0 }} \
                 elseif ($_.VoiceInfo.Culture.Name -eq 'en-IN') {{ 1 }} else {{ 2 }} \
               }}); \
             if ($voices.Count -eq 0) {{ \
               [Console]::Error.WriteLine('No installed English Windows voice. Install an English voice in Windows Settings > Time & language > Speech.'); \
               exit 23 \
             }}; \
             $s.SelectVoice($voices[0].VoiceInfo.Name); \
             $s.Rate = {sapi_rate}; $s.Volume = {sapi_volume}; \
             $s.Speak([Console]::In.ReadToEnd()); $s.Dispose()"
        );
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &script,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = platform::no_console(&mut command)
            .spawn()
            .map_err(|error| format!("could not start Windows SAPI: {error}"))?;
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| "could not open SAPI input.".to_string())?
            .write_all(text.as_bytes());
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not pass text to SAPI: {error}"));
        }
        *process.child.lock().unwrap() = Some(child);
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let _ = text;
        Err("Windows SAPI is available only on Windows.".into())
    }
}

#[tauri::command]
pub fn tts_stop(process: tauri::State<'_, TtsProcess>) -> Result<(), String> {
    process.stop()
}

#[tauri::command]
pub fn tts_is_speaking(process: tauri::State<'_, TtsProcess>) -> Result<bool, String> {
    let mut child = process.child.lock().unwrap();
    let Some(running) = child.as_mut() else {
        return Ok(false);
    };
    match running.try_wait() {
        Ok(None) => Ok(true),
        Ok(Some(status)) if status.success() => {
            *child = None;
            Ok(false)
        }
        Ok(Some(status)) if status.code() == Some(23) => {
            *child = None;
            Err("No installed English Windows voice. Install an English voice in Windows Settings > Time & language > Speech.".into())
        }
        Ok(Some(status)) => {
            *child = None;
            Err(format!("Windows SAPI exited with {status}."))
        }
        Err(error) => Err(format!("could not check Windows SAPI: {error}")),
    }
}

pub fn start_stt_server(app: &AppHandle, process: &SttProcess) {
    let Some(script) = stt_script_path(app) else {
        log::line("voice STT server script was not found; voice input is unavailable");
        return;
    };
    process.stopping.store(false, Ordering::Relaxed);

    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut venv_candidates = vec![project_root.join(".venv/Scripts/python.exe")];
    if let Ok(current) = std::env::current_dir() {
        venv_candidates.push(current.join(".venv/Scripts/python.exe"));
    }
    if let Some(root) = script.parent() {
        venv_candidates.push(root.join(".venv/Scripts/python.exe"));
    }
    let venv_python = venv_candidates.into_iter().find(|path| path.is_file());
    if let Some(python) = venv_python {
        let mut check = Command::new(&python);
        if dependencies_installed(&mut check) {
            let mut command = Command::new(python);
            command.arg(&script);
            if launch(&mut command, "project virtual environment", app, process) {
                return;
            }
        } else {
            log::line("voice STT virtual environment is missing Flask or faster-whisper");
        }
    }

    let launchers: [(&str, &[&str]); 2] = [("python", &[]), ("py", &["-3"])];
    let mut failures = Vec::new();
    for (program, prefix) in launchers {
        let mut check = Command::new(program);
        check.args(prefix);
        if !dependencies_installed(&mut check) {
            failures.push(format!(
                "{program}: Python or voice dependencies unavailable"
            ));
            continue;
        }
        let mut command = Command::new(program);
        command.args(prefix).arg(&script);
        if launch(&mut command, program, app, process) {
            return;
        }
        failures.push(format!("{program}: could not spawn the STT server"));
    }
    log::line(format!(
        "voice STT server failed to start: {}",
        failures.join("; ")
    ));
}

fn dependencies_installed(command: &mut Command) -> bool {
    command
        .args(["-c", "import flask, faster_whisper"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    platform::no_console(command)
        .status()
        .is_ok_and(|status| status.success())
}

fn launch(command: &mut Command, label: &str, app: &AppHandle, process: &SttProcess) -> bool {
    #[cfg(windows)]
    let job = match create_kill_on_close_job() {
        Ok(job) => job,
        Err(error) => {
            log::line(format!("could not supervise voice STT processes: {error}"));
            return false;
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match platform::no_console(command).spawn() {
        Ok(child) => {
            #[cfg(windows)]
            let child = match assign_to_job(child.id(), &job) {
                Ok(()) => {
                    *process.job.lock().unwrap() = Some(job);
                    child
                }
                Err(error) => {
                    log::line(format!("could not supervise voice STT process: {error}"));
                    let mut child = child;
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            };

            let mut child = child;
            if let Some(stderr) = child.stderr.take() {
                std::thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().flatten() {
                        let lower = line.to_ascii_lowercase();
                        if [
                            "traceback",
                            "error",
                            "exception",
                            "failed",
                            "modulenotfound",
                        ]
                        .iter()
                        .any(|term| lower.contains(term))
                        {
                            log::line(format!("voice STT: {line}"));
                        }
                    }
                });
            }
            log::line(format!("voice STT server launched with {label}"));
            *process.child.lock().unwrap() = Some(child);
            monitor(app.clone());
            true
        }
        Err(error) => {
            log::line(format!(
                "voice STT server could not launch with {label}: {error}"
            ));
            false
        }
    }
}

fn monitor(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(2));
        let process = app.state::<SttProcess>();
        if process.stopping.load(Ordering::Relaxed) {
            return;
        }
        let mut child = process.child.lock().unwrap();
        let Some(running_child) = child.as_mut() else {
            return;
        };
        let status = running_child.try_wait();
        match status {
            Ok(Some(status)) => {
                log::line(format!("voice STT server exited unexpectedly: {status}"));
                *child = None;
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log::line(format!("could not check voice STT server process: {error}"));
                return;
            }
        }
    });
}

pub fn stop_stt_server(process: &SttProcess) {
    process.stopping.store(true, Ordering::Relaxed);
    #[cfg(windows)]
    drop(process.job.lock().unwrap().take());
    let mut child = process.child.lock().unwrap().take();
    if let Some(child) = child.as_mut() {
        #[cfg(not(windows))]
        if let Err(error) = child.kill() {
            log::line(format!("voice STT server shutdown failed: {error}"));
        }
        let _ = child.wait();
    }
}

#[cfg(windows)]
fn create_kill_on_close_job() -> Result<OwnedHandle, String> {
    use std::mem::size_of;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|error| format!("CreateJobObjectW failed: {error}"))?;
    let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let raw = HANDLE(owned.as_raw_handle());
    unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .map_err(|error| format!("SetInformationJobObject failed: {error}"))?;
    Ok(owned)
}

#[cfg(windows)]
fn assign_to_job(process_id: u32, job: &OwnedHandle) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, process_id) }
        .map_err(|error| format!("OpenProcess failed: {error}"))?;
    let result = unsafe {
        AssignProcessToJobObject(
            windows::Win32::Foundation::HANDLE(job.as_raw_handle()),
            process,
        )
    }
    .map_err(|error| format!("AssignProcessToJobObject failed: {error}"));
    let _ = unsafe { CloseHandle(process) };
    result
}

fn stt_script_path(app: &AppHandle) -> Option<PathBuf> {
    let source_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stt_server.py");
    if source_script.is_file() {
        return Some(source_script);
    }

    let mut candidates = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        candidates.push(resources.join("stt_server.py"));
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current.join("stt_server.py"));
        candidates.push(current.join("../stt_server.py"));
    }
    candidates.into_iter().find(|path| path.is_file())
}
