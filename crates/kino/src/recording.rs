pub(crate) use imp::{record_command, record_ssh};

#[cfg(unix)]
mod imp {
    use crate::config::RecordingConfig;
    use anyhow::{Context, Result, anyhow, bail};
    use crossterm::terminal;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use serde::Serialize;
    use signal_hook::consts::signal::SIGWINCH;
    use signal_hook::iterator::{Handle as SignalsHandle, Signals};
    use std::collections::BTreeMap;
    use std::fs::{self, File, OpenOptions};
    use std::io::ErrorKind;
    use std::io::{self, IsTerminal, Read, Write};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const DEFAULT_TTY_WIDTH: u16 = 80;
    const DEFAULT_TTY_HEIGHT: u16 = 24;
    const CAST_SYNC_INTERVAL_MS: u64 = 250;

    #[derive(Debug, Serialize)]
    struct CastHeader {
        version: u8,
        width: u16,
        height: u16,
        timestamp: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    }

    #[derive(Debug, Clone, Default)]
    struct CastMetadata {
        command: Option<String>,
        env: BTreeMap<String, String>,
    }

    struct CastWriter {
        file: File,
        start_ts_unix_ms: u64,
        last_sync_ts_unix_ms: u64,
        pending_input_bytes: Vec<u8>,
        pending_output_bytes: Vec<u8>,
    }

    impl CastWriter {
        fn start(
            output_dir: &Path,
            start_ts_unix_ms: u64,
            width: u16,
            height: u16,
            metadata: CastMetadata,
        ) -> io::Result<(Self, PathBuf)> {
            fs::create_dir_all(output_dir)?;

            let (file, path) = create_session_file(output_dir, start_ts_unix_ms)?;
            let mut writer = Self {
                file,
                start_ts_unix_ms,
                last_sync_ts_unix_ms: start_ts_unix_ms,
                pending_input_bytes: Vec::new(),
                pending_output_bytes: Vec::new(),
            };

            let header = CastHeader {
                version: 2,
                width,
                height,
                timestamp: start_ts_unix_ms / 1000,
                command: metadata.command,
                env: metadata.env,
            };
            let line = serde_json::to_string(&header).map_err(io::Error::other)?;
            writer.file.write_all(line.as_bytes())?;
            writer.file.write_all(b"\n")?;
            writer.file.sync_data()?;

            Ok((writer, path))
        }

        fn write_input_bytes(&mut self, ts_unix_ms: u64, bytes: &[u8]) -> io::Result<()> {
            Self::write_stream_event(
                &mut self.file,
                self.start_ts_unix_ms,
                &mut self.last_sync_ts_unix_ms,
                &mut self.pending_input_bytes,
                ts_unix_ms,
                "i",
                bytes,
            )
        }

        fn write_output_bytes(&mut self, ts_unix_ms: u64, bytes: &[u8]) -> io::Result<()> {
            Self::write_stream_event(
                &mut self.file,
                self.start_ts_unix_ms,
                &mut self.last_sync_ts_unix_ms,
                &mut self.pending_output_bytes,
                ts_unix_ms,
                "o",
                bytes,
            )
        }

        fn write_resize(&mut self, ts_unix_ms: u64, width: u16, height: u16) -> io::Result<()> {
            let data = format!("{width}x{height}");
            self.write_event_text(ts_unix_ms, "r", &data)
        }

        fn finish(&mut self, ts_unix_ms: u64) -> io::Result<()> {
            self.flush_pending_stream_bytes(ts_unix_ms)?;
            self.file.sync_all()
        }

        fn write_event_text(
            &mut self,
            ts_unix_ms: u64,
            kind: &'static str,
            data: &str,
        ) -> io::Result<()> {
            let rel = Duration::from_millis(ts_unix_ms.saturating_sub(self.start_ts_unix_ms))
                .as_secs_f64();
            let line = serde_json::to_string(&(rel, kind, data)).map_err(io::Error::other)?;
            self.file.write_all(line.as_bytes())?;
            self.file.write_all(b"\n")?;
            self.sync_data_if_due(ts_unix_ms)?;
            Ok(())
        }

        fn flush_pending_stream_bytes(&mut self, ts_unix_ms: u64) -> io::Result<()> {
            Self::flush_pending_bytes(
                &mut self.file,
                self.start_ts_unix_ms,
                &mut self.last_sync_ts_unix_ms,
                &mut self.pending_input_bytes,
                ts_unix_ms,
                "i",
            )?;
            Self::flush_pending_bytes(
                &mut self.file,
                self.start_ts_unix_ms,
                &mut self.last_sync_ts_unix_ms,
                &mut self.pending_output_bytes,
                ts_unix_ms,
                "o",
            )?;
            Ok(())
        }

        fn sync_data_if_due(&mut self, ts_unix_ms: u64) -> io::Result<()> {
            if ts_unix_ms.saturating_sub(self.last_sync_ts_unix_ms) < CAST_SYNC_INTERVAL_MS {
                return Ok(());
            }
            self.file.sync_data()?;
            self.last_sync_ts_unix_ms = ts_unix_ms;
            Ok(())
        }

        fn write_stream_event(
            file: &mut File,
            start_ts_unix_ms: u64,
            last_sync_ts_unix_ms: &mut u64,
            pending: &mut Vec<u8>,
            ts_unix_ms: u64,
            kind: &'static str,
            bytes: &[u8],
        ) -> io::Result<()> {
            pending.extend_from_slice(bytes);

            loop {
                match std::str::from_utf8(pending) {
                    Ok(valid) => {
                        if !valid.is_empty() {
                            Self::write_event_line(
                                file,
                                start_ts_unix_ms,
                                last_sync_ts_unix_ms,
                                ts_unix_ms,
                                kind,
                                valid,
                            )?;
                        }
                        pending.clear();
                        return Ok(());
                    }
                    Err(error) => {
                        let valid_up_to = error.valid_up_to();
                        if valid_up_to > 0 {
                            let valid =
                                String::from_utf8_lossy(&pending[..valid_up_to]).into_owned();
                            Self::write_event_line(
                                file,
                                start_ts_unix_ms,
                                last_sync_ts_unix_ms,
                                ts_unix_ms,
                                kind,
                                &valid,
                            )?;
                        }

                        if let Some(invalid_len) = error.error_len() {
                            Self::write_event_line(
                                file,
                                start_ts_unix_ms,
                                last_sync_ts_unix_ms,
                                ts_unix_ms,
                                kind,
                                "\u{FFFD}",
                            )?;
                            pending.drain(..valid_up_to + invalid_len);
                        } else {
                            if valid_up_to > 0 {
                                pending.drain(..valid_up_to);
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }

        fn flush_pending_bytes(
            file: &mut File,
            start_ts_unix_ms: u64,
            last_sync_ts_unix_ms: &mut u64,
            pending: &mut Vec<u8>,
            ts_unix_ms: u64,
            kind: &'static str,
        ) -> io::Result<()> {
            if pending.is_empty() {
                return Ok(());
            }
            let data = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            if data.is_empty() {
                return Ok(());
            }
            Self::write_event_line(
                file,
                start_ts_unix_ms,
                last_sync_ts_unix_ms,
                ts_unix_ms,
                kind,
                &data,
            )
        }

        fn write_event_line(
            file: &mut File,
            start_ts_unix_ms: u64,
            last_sync_ts_unix_ms: &mut u64,
            ts_unix_ms: u64,
            kind: &'static str,
            data: &str,
        ) -> io::Result<()> {
            let rel =
                Duration::from_millis(ts_unix_ms.saturating_sub(start_ts_unix_ms)).as_secs_f64();
            let line = serde_json::to_string(&(rel, kind, data)).map_err(io::Error::other)?;
            file.write_all(line.as_bytes())?;
            file.write_all(b"\n")?;
            if ts_unix_ms.saturating_sub(*last_sync_ts_unix_ms) >= CAST_SYNC_INTERVAL_MS {
                file.sync_data()?;
                *last_sync_ts_unix_ms = ts_unix_ms;
            }
            Ok(())
        }
    }

    pub(crate) fn record_command(config: &RecordingConfig, command: &str) -> Result<i32> {
        let start_ts_unix_ms = unix_ms();
        let (width, height) = terminal::size().unwrap_or((DEFAULT_TTY_WIDTH, DEFAULT_TTY_HEIGHT));
        let metadata =
            build_cast_metadata(config, (!command.is_empty()).then(|| command.to_owned()));
        let (mut writer, cast_path) = CastWriter::start(
            &config.output_dir,
            start_ts_unix_ms,
            width,
            height,
            metadata,
        )
        .with_context(|| {
            format!(
                "failed to create cast file in {}",
                config.output_dir.display()
            )
        })?;

        if !command.is_empty() {
            let mut input = command.to_owned();
            input.push('\n');
            writer
                .write_input_bytes(unix_ms(), input.as_bytes())
                .with_context(|| {
                    format!("failed to write input event to {}", cast_path.display())
                })?;
        }

        let output = Command::new(&config.real_shell)
            .args(["-c", command])
            .output()
            .with_context(|| {
                format!(
                    "failed to run shell command via {}",
                    config.real_shell.display()
                )
            })?;
        let exit_code = output.status.code().unwrap_or(1);

        if !output.stdout.is_empty() {
            writer
                .write_output_bytes(unix_ms(), &output.stdout)
                .with_context(|| {
                    format!("failed to write stdout event to {}", cast_path.display())
                })?;
        }
        if !output.stderr.is_empty() {
            writer
                .write_output_bytes(unix_ms(), &output.stderr)
                .with_context(|| {
                    format!("failed to write stderr event to {}", cast_path.display())
                })?;
        }

        writer
            .finish(unix_ms())
            .with_context(|| format!("failed to flush cast file {}", cast_path.display()))?;

        Ok(exit_code)
    }

    pub(crate) fn record_ssh(config: &RecordingConfig) -> Result<i32> {
        ensure_interactive_tty()?;

        let (width, height) = tty_dimensions();
        let (shared_writer, cast_path, _raw_mode) =
            prepare_interactive_writer(config, width, height)?;
        let input_error = Arc::new(Mutex::new(None::<String>));
        let resize_error = Arc::new(Mutex::new(None::<String>));
        let (mut child, mut pty_reader, pty_writer, pty_master) =
            start_login_shell(&config.real_shell, width, height)?;

        spawn_input_forwarder(
            pty_writer,
            Arc::clone(&shared_writer),
            Arc::clone(&input_error),
        );
        let resize_handle = spawn_resize_forwarder(
            pty_master,
            Arc::clone(&shared_writer),
            Arc::clone(&resize_error),
        )?;

        let output_result = proxy_shell_output(&mut pty_reader, &shared_writer);
        if output_result.is_err() {
            let _ = child.kill();
        }

        let status = child.wait().context("failed waiting for login shell")?;
        let exit_code = i32::try_from(status.exit_code()).unwrap_or(1);
        resize_handle.close();

        if let Some(message) = take_thread_error(&input_error) {
            return Err(anyhow!(message));
        }
        if let Some(message) = take_thread_error(&resize_error) {
            return Err(anyhow!(message));
        }

        output_result?;

        {
            let mut writer = shared_writer
                .lock()
                .map_err(|_| anyhow!("cast writer lock poisoned"))?;
            writer
                .finish(unix_ms())
                .with_context(|| format!("failed to flush cast file {}", cast_path.display()))?;
        }

        Ok(exit_code)
    }

    fn ensure_interactive_tty() -> Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();

        if !stdin.is_terminal() || !stdout.is_terminal() {
            bail!("interactive recording requires a TTY");
        }

        Ok(())
    }

    fn tty_dimensions() -> (u16, u16) {
        terminal::size().unwrap_or((DEFAULT_TTY_WIDTH, DEFAULT_TTY_HEIGHT))
    }

    fn prepare_interactive_writer(
        config: &RecordingConfig,
        width: u16,
        height: u16,
    ) -> Result<(Arc<Mutex<CastWriter>>, PathBuf, RawModeGuard)> {
        let start_ts_unix_ms = unix_ms();
        let metadata = build_cast_metadata(
            config,
            Some(config.real_shell.to_string_lossy().into_owned()),
        );
        let (writer, cast_path) = CastWriter::start(
            &config.output_dir,
            start_ts_unix_ms,
            width,
            height,
            metadata,
        )
        .with_context(|| {
            format!(
                "failed to create cast file in {}",
                config.output_dir.display()
            )
        })?;
        let raw_mode = RawModeGuard::new()?;

        Ok((Arc::new(Mutex::new(writer)), cast_path, raw_mode))
    }

    type PtyChild = Box<dyn portable_pty::Child + Send>;
    type PtyReader = Box<dyn Read + Send>;
    type PtyMaster = Box<dyn portable_pty::MasterPty + Send>;
    type PtyWriter = Box<dyn Write + Send>;

    fn start_login_shell(
        real_shell: &Path,
        width: u16,
        height: u16,
    ) -> Result<(PtyChild, PtyReader, PtyWriter, PtyMaster)> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: height,
                cols: width,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to allocate PTY")?;

        let mut command = CommandBuilder::new(real_shell.to_string_lossy().into_owned());
        command.arg("-l");

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to launch login shell {}", real_shell.display()))?;
        let pty_reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let pty_writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;

        Ok((child, pty_reader, pty_writer, pair.master))
    }

    fn spawn_input_forwarder(
        mut pty_writer: PtyWriter,
        writer: Arc<Mutex<CastWriter>>,
        error_slot: Arc<Mutex<Option<String>>>,
    ) {
        let _input_thread = thread::spawn(move || {
            let mut input = io::stdin();
            let mut buffer = [0_u8; 4096];

            loop {
                match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read_count) => {
                        if let Err(error) = pty_writer
                            .write_all(&buffer[..read_count])
                            .and_then(|()| pty_writer.flush())
                        {
                            if is_expected_pty_shutdown_error(&error) {
                                break;
                            }
                            store_thread_error(
                                &error_slot,
                                format!("failed to forward input to shell: {error}"),
                            );
                            break;
                        }
                        if let Err(error) =
                            write_cast_chunk(&writer, unix_ms(), "i", &buffer[..read_count])
                        {
                            store_thread_error(
                                &error_slot,
                                format!("failed to write input event: {error}"),
                            );
                            break;
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(error) => {
                        store_thread_error(&error_slot, format!("failed to read stdin: {error}"));
                        break;
                    }
                }
            }
        });
    }

    fn spawn_resize_forwarder(
        pty_master: PtyMaster,
        writer: Arc<Mutex<CastWriter>>,
        error_slot: Arc<Mutex<Option<String>>>,
    ) -> Result<SignalsHandle> {
        let mut signals = Signals::new([SIGWINCH]).context("failed to subscribe to SIGWINCH")?;
        let handle = signals.handle();

        let _resize_thread = thread::spawn(move || {
            for _ in signals.forever() {
                let (width, height) = tty_dimensions();
                let size = PtySize {
                    rows: height,
                    cols: width,
                    pixel_width: 0,
                    pixel_height: 0,
                };
                if let Err(error) = pty_master.resize(size) {
                    if is_expected_pty_shutdown_anyhow(&error) {
                        break;
                    }
                    store_thread_error(&error_slot, format!("failed to resize PTY: {error}"));
                    break;
                }
                if let Err(error) = write_resize_event(&writer, unix_ms(), width, height) {
                    store_thread_error(
                        &error_slot,
                        format!("failed to write resize event: {error}"),
                    );
                    break;
                }
            }
        });

        Ok(handle)
    }

    fn create_session_file(
        output_dir: &Path,
        start_ts_unix_ms: u64,
    ) -> io::Result<(File, PathBuf)> {
        let pid = std::process::id();

        for attempt in 0..1000_u32 {
            let suffix = if attempt == 0 {
                String::new()
            } else {
                format!("-{attempt}")
            };
            let path =
                output_dir.join(format!("ssh-session-{start_ts_unix_ms}-{pid}{suffix}.cast"));

            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok((file, path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to allocate a unique cast file path",
        ))
    }

    struct RawModeGuard;

    impl RawModeGuard {
        fn new() -> Result<Self> {
            terminal::enable_raw_mode()
                .map_err(io::Error::other)
                .context("failed to enable raw terminal mode")?;
            Ok(Self)
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
        }
    }

    fn proxy_shell_output(
        pty_reader: &mut dyn Read,
        writer: &Arc<Mutex<CastWriter>>,
    ) -> Result<()> {
        let mut stdout = io::stdout();
        let mut buffer = [0_u8; 4096];

        loop {
            match pty_reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read_count) => {
                    stdout
                        .write_all(&buffer[..read_count])
                        .context("failed to forward shell output to stdout")?;
                    stdout.flush().context("failed to flush stdout")?;
                    write_cast_chunk(writer, unix_ms(), "o", &buffer[..read_count])
                        .context("failed to write output event")?;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if is_expected_pty_shutdown_error(&error) => break,
                Err(error) => return Err(error).context("failed to read PTY output"),
            }
        }

        Ok(())
    }

    fn is_expected_pty_shutdown_error(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            ErrorKind::BrokenPipe
                | ErrorKind::ConnectionReset
                | ErrorKind::UnexpectedEof
                | ErrorKind::WouldBlock
        ) || matches!(error.raw_os_error(), Some(5 | 32))
    }

    fn is_expected_pty_shutdown_anyhow(error: &anyhow::Error) -> bool {
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<io::Error>())
            .is_some_and(is_expected_pty_shutdown_error)
    }

    fn write_resize_event(
        writer: &Arc<Mutex<CastWriter>>,
        ts_unix_ms: u64,
        width: u16,
        height: u16,
    ) -> Result<()> {
        let mut writer = writer
            .lock()
            .map_err(|_| anyhow!("cast writer lock poisoned"))?;
        writer.write_resize(ts_unix_ms, width, height)?;
        Ok(())
    }

    fn write_cast_chunk(
        writer: &Arc<Mutex<CastWriter>>,
        ts_unix_ms: u64,
        kind: &'static str,
        bytes: &[u8],
    ) -> Result<()> {
        let mut writer = writer
            .lock()
            .map_err(|_| anyhow!("cast writer lock poisoned"))?;
        match kind {
            "i" => writer.write_input_bytes(ts_unix_ms, bytes)?,
            "o" => writer.write_output_bytes(ts_unix_ms, bytes)?,
            _ => bail!("unsupported cast event kind '{kind}'"),
        }
        Ok(())
    }

    fn store_thread_error(slot: &Arc<Mutex<Option<String>>>, message: String) {
        if let Ok(mut guard) = slot.lock()
            && guard.is_none()
        {
            *guard = Some(message);
        }
    }

    fn take_thread_error(slot: &Arc<Mutex<Option<String>>>) -> Option<String> {
        slot.lock().ok().and_then(|mut guard| guard.take())
    }

    fn build_cast_metadata(config: &RecordingConfig, command: Option<String>) -> CastMetadata {
        let mut env = BTreeMap::new();
        env.insert(
            "SHELL".to_owned(),
            config.real_shell.to_string_lossy().into_owned(),
        );

        if let Ok(term) = std::env::var("TERM")
            && !term.is_empty()
        {
            env.insert("TERM".to_owned(), term);
        }

        CastMetadata { command, env }
    }

    fn unix_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    mod tests {
        use super::{CastMetadata, CastWriter, record_command};
        use crate::config::RecordingConfig;
        use serde_json::Value;
        use std::collections::BTreeMap;
        use std::fs;
        use std::path::PathBuf;

        #[test]
        fn cast_writer_persists_header_and_events() {
            let temp =
                tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
            let start_ts_unix_ms = 1_700_000_000_000;
            let metadata = CastMetadata {
                command: Some("/bin/sh".to_owned()),
                env: BTreeMap::from([
                    ("SHELL".to_owned(), "/bin/sh".to_owned()),
                    ("TERM".to_owned(), "xterm-256color".to_owned()),
                ]),
            };
            let (mut writer, cast_path) =
                CastWriter::start(temp.path(), start_ts_unix_ms, 120, 40, metadata)
                    .unwrap_or_else(|error| panic!("cast writer start failed: {error}"));

            writer
                .write_input_bytes(start_ts_unix_ms, b"echo hello\n")
                .unwrap_or_else(|error| panic!("write input failed: {error}"));
            writer
                .write_output_bytes(start_ts_unix_ms + 500, b"hello\n")
                .unwrap_or_else(|error| panic!("write output failed: {error}"));
            writer
                .write_resize(start_ts_unix_ms + 900, 100, 30)
                .unwrap_or_else(|error| panic!("write resize failed: {error}"));
            writer
                .finish(start_ts_unix_ms + 900)
                .unwrap_or_else(|error| panic!("finish failed: {error}"));

            let content = fs::read_to_string(cast_path)
                .unwrap_or_else(|error| panic!("failed to read cast file: {error}"));
            let lines = content.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), 4);

            let header = serde_json::from_str::<Value>(lines[0])
                .unwrap_or_else(|error| panic!("invalid cast header: {error}"));
            assert_eq!(header["version"], 2);
            assert_eq!(header["width"], 120);
            assert_eq!(header["height"], 40);
            assert_eq!(header["timestamp"], 1_700_000_000_u64);
            assert_eq!(header["command"], "/bin/sh");
            assert_eq!(header["env"]["SHELL"], "/bin/sh");
            assert_eq!(header["env"]["TERM"], "xterm-256color");

            let input = serde_json::from_str::<Value>(lines[1])
                .unwrap_or_else(|error| panic!("invalid input event: {error}"));
            assert_eq!(input[0].as_f64(), Some(0.0));
            assert_eq!(input[1], "i");
            assert_eq!(input[2], "echo hello\n");

            let output = serde_json::from_str::<Value>(lines[2])
                .unwrap_or_else(|error| panic!("invalid output event: {error}"));
            assert_eq!(output[1], "o");
            assert_eq!(output[2], "hello\n");

            let resize = serde_json::from_str::<Value>(lines[3])
                .unwrap_or_else(|error| panic!("invalid resize event: {error}"));
            assert_eq!(resize[1], "r");
            assert_eq!(resize[2], "100x30");
        }

        #[test]
        fn record_command_creates_cast_file() {
            let temp =
                tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
            let config = RecordingConfig {
                output_dir: temp.path().to_path_buf(),
                real_shell: PathBuf::from("/bin/sh"),
            };

            let exit_code = record_command(&config, "printf 'hello\\n'; >&2 printf 'oops\\n'")
                .unwrap_or_else(|error| panic!("record_command failed: {error}"));
            assert_eq!(exit_code, 0);

            let entries = fs::read_dir(temp.path())
                .unwrap_or_else(|error| panic!("read_dir failed: {error}"))
                .map(|entry| {
                    entry
                        .unwrap_or_else(|error| panic!("dir entry failed: {error}"))
                        .path()
                })
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 1);

            let content = fs::read_to_string(&entries[0])
                .unwrap_or_else(|error| panic!("failed to read cast file: {error}"));
            let lines = content.lines().skip(1).collect::<Vec<_>>();
            let events = lines
                .iter()
                .map(|line| {
                    serde_json::from_str::<Value>(line)
                        .unwrap_or_else(|error| panic!("invalid cast line: {error}"))
                })
                .collect::<Vec<_>>();

            assert!(events.iter().any(|event| {
                event[1] == "i"
                    && event[2]
                        .as_str()
                        .is_some_and(|data| data.contains("printf 'hello"))
            }));
            assert!(events.iter().all(|event| {
                event[0].is_number()
                    && matches!(event[1].as_str(), Some("i" | "o" | "r" | "m"))
                    && event[2].is_string()
            }));
            assert!(
                events
                    .iter()
                    .any(|event| event[1] == "o" && event[2].as_str() == Some("hello\n"))
            );
            assert!(
                events
                    .iter()
                    .any(|event| event[1] == "o" && event[2].as_str() == Some("oops\n"))
            );
        }

        #[test]
        fn record_command_fails_closed_when_output_dir_is_a_file() {
            let temp =
                tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir failed: {error}"));
            let output_path = temp.path().join("not-a-directory");
            fs::write(&output_path, "occupied")
                .unwrap_or_else(|error| panic!("write failed: {error}"));

            let config = RecordingConfig {
                output_dir: output_path,
                real_shell: PathBuf::from("/bin/sh"),
            };

            let result = record_command(&config, "printf 'hello\\n'");
            assert!(result.is_err());
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use crate::config::RecordingConfig;
    use anyhow::{Result, bail};

    pub(crate) fn record_command(_config: &RecordingConfig, _command: &str) -> Result<i32> {
        bail!("recording is only supported on Unix platforms")
    }

    pub(crate) fn record_ssh(_config: &RecordingConfig) -> Result<i32> {
        bail!("recording is only supported on Unix platforms")
    }
}
