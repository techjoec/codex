#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
use std::time::Instant;

use async_channel::Sender;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::process::Child;

use crate::bash::parse_bash_lc_plain_commands;
use crate::error::CodexErr;
use crate::error::Result;
use crate::error::SandboxErr;
use crate::landlock::spawn_command_under_linux_sandbox;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecCommandOutputDeltaEvent;
use crate::protocol::ExecOutputStream;
use crate::protocol::SandboxPolicy;
use crate::seatbelt::spawn_command_under_seatbelt;
use crate::spawn::StdioPolicy;
use crate::spawn::spawn_child_async;

const DEFAULT_TIMEOUT_MS: u64 = 10_000;

// Hardcode these since it does not seem worth including the libc crate just
// for these.
const SIGKILL_CODE: i32 = 9;
const TIMEOUT_CODE: i32 = 64;
const EXIT_CODE_SIGNAL_BASE: i32 = 128; // conventional shell: 128 + signal
const EXEC_TIMEOUT_EXIT_CODE: i32 = 124; // conventional timeout exit code

// I/O buffer sizing
const READ_CHUNK_SIZE: usize = 8192; // bytes per read
const AGGREGATE_BUFFER_INITIAL_CAPACITY: usize = 8 * 1024; // 8 KiB

const GENERIC_EXEC_OUTPUT_MAX_BYTES: usize = 6 * 1024; // 6 KiB budget for most commands
const RG_EXEC_OUTPUT_MAX_BYTES: usize = 8 * 1024; // 8 KiB budget for ripgrep

const GENERIC_EXEC_TRUNCATION_NOTICE: &str =
    "[output truncated to 6 KiB; refine the command or request /relax for a temporary increase]";
const RG_EXEC_TRUNCATION_NOTICE: &str =
    "[rg output truncated to 8 KiB; narrow the search (e.g., add filters) or request /relax]";

#[derive(Clone, Copy, Debug)]
struct ExecOutputLimit {
    stream_max_bytes: usize,
    aggregated_max_bytes: usize,
    truncation_notice: &'static str,
}

impl ExecOutputLimit {
    const fn generic() -> Self {
        Self {
            stream_max_bytes: GENERIC_EXEC_OUTPUT_MAX_BYTES,
            aggregated_max_bytes: GENERIC_EXEC_OUTPUT_MAX_BYTES,
            truncation_notice: GENERIC_EXEC_TRUNCATION_NOTICE,
        }
    }

    const fn ripgrep() -> Self {
        Self {
            stream_max_bytes: RG_EXEC_OUTPUT_MAX_BYTES,
            aggregated_max_bytes: RG_EXEC_OUTPUT_MAX_BYTES,
            truncation_notice: RG_EXEC_TRUNCATION_NOTICE,
        }
    }
}

fn exec_output_limit_for_command(command: &[String]) -> ExecOutputLimit {
    if command_invokes_ripgrep(command) {
        ExecOutputLimit::ripgrep()
    } else {
        ExecOutputLimit::generic()
    }
}

fn command_invokes_ripgrep(command: &[String]) -> bool {
    fn is_rg_program(program: &str) -> bool {
        Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|name| name == "rg")
            .unwrap_or(false)
    }

    if let Some(all_commands) = parse_bash_lc_plain_commands(command) {
        if all_commands.len() != 1 {
            return false;
        }
        all_commands
            .first()
            .and_then(|cmd| cmd.first())
            .map(|program| is_rg_program(program))
            .unwrap_or(false)
    } else {
        command
            .first()
            .map(|program| is_rg_program(program))
            .unwrap_or(false)
    }
}

/// Limit the number of ExecCommandOutputDelta events emitted per exec call.
/// Aggregation still collects full output; only the live event stream is capped.
pub(crate) const MAX_EXEC_OUTPUT_DELTAS_PER_CALL: usize = 10_000;

#[derive(Clone, Debug)]
pub struct ExecParams {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: Option<u64>,
    pub env: HashMap<String, String>,
    pub with_escalated_permissions: Option<bool>,
    pub justification: Option<String>,
}

impl ExecParams {
    pub fn timeout_duration(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SandboxType {
    None,

    /// Only available on macOS.
    MacosSeatbelt,

    /// Only available on Linux.
    LinuxSeccomp,
}

#[derive(Clone)]
pub struct StdoutStream {
    pub sub_id: String,
    pub call_id: String,
    pub tx_event: Sender<Event>,
}

pub async fn process_exec_tool_call(
    params: ExecParams,
    sandbox_type: SandboxType,
    sandbox_policy: &SandboxPolicy,
    sandbox_cwd: &Path,
    codex_linux_sandbox_exe: &Option<PathBuf>,
    stdout_stream: Option<StdoutStream>,
) -> Result<ExecToolCallOutput> {
    let start = Instant::now();

    let timeout_duration = params.timeout_duration();
    let output_limit = exec_output_limit_for_command(&params.command);

    let raw_output_result: std::result::Result<RawExecToolCallOutput, CodexErr> = match sandbox_type
    {
        SandboxType::None => {
            exec(params, sandbox_policy, stdout_stream.clone(), output_limit).await
        }
        SandboxType::MacosSeatbelt => {
            let ExecParams {
                command,
                cwd: command_cwd,
                env,
                ..
            } = params;
            let child = spawn_command_under_seatbelt(
                command,
                command_cwd,
                sandbox_policy,
                sandbox_cwd,
                StdioPolicy::RedirectForShellTool,
                env,
            )
            .await?;
            consume_truncated_output(child, timeout_duration, stdout_stream.clone(), output_limit)
                .await
        }
        SandboxType::LinuxSeccomp => {
            let ExecParams {
                command,
                cwd: command_cwd,
                env,
                ..
            } = params;

            let codex_linux_sandbox_exe = codex_linux_sandbox_exe
                .as_ref()
                .ok_or(CodexErr::LandlockSandboxExecutableNotProvided)?;
            let child = spawn_command_under_linux_sandbox(
                codex_linux_sandbox_exe,
                command,
                command_cwd,
                sandbox_policy,
                sandbox_cwd,
                StdioPolicy::RedirectForShellTool,
                env,
            )
            .await?;

            consume_truncated_output(child, timeout_duration, stdout_stream, output_limit).await
        }
    };
    let duration = start.elapsed();
    match raw_output_result {
        Ok(raw_output) => {
            #[allow(unused_mut)]
            let mut timed_out = raw_output.timed_out;

            #[cfg(target_family = "unix")]
            {
                if let Some(signal) = raw_output.exit_status.signal() {
                    if signal == TIMEOUT_CODE {
                        timed_out = true;
                    } else {
                        return Err(CodexErr::Sandbox(SandboxErr::Signal(signal)));
                    }
                }
            }

            let mut exit_code = raw_output.exit_status.code().unwrap_or(-1);
            if timed_out {
                exit_code = EXEC_TIMEOUT_EXIT_CODE;
            }

            let mut stdout = raw_output.stdout.from_utf8_lossy();
            let mut stderr = raw_output.stderr.from_utf8_lossy();
            let mut aggregated_output = raw_output.aggregated_output.from_utf8_lossy();

            if stdout.truncated_by_bytes || stderr.truncated_by_bytes {
                aggregated_output.truncated_by_bytes = true;
            }

            append_truncation_notice(&mut stdout, output_limit.truncation_notice);
            append_truncation_notice(&mut stderr, output_limit.truncation_notice);
            append_truncation_notice(&mut aggregated_output, output_limit.truncation_notice);

            let exec_output = ExecToolCallOutput {
                exit_code,
                stdout,
                stderr,
                aggregated_output,
                duration,
                timed_out,
            };

            if timed_out {
                return Err(CodexErr::Sandbox(SandboxErr::Timeout {
                    output: Box::new(exec_output),
                }));
            }

            if exit_code != 0 && is_likely_sandbox_denied(sandbox_type, exit_code) {
                return Err(CodexErr::Sandbox(SandboxErr::Denied {
                    output: Box::new(exec_output),
                }));
            }

            Ok(exec_output)
        }
        Err(err) => {
            tracing::error!("exec error: {err}");
            Err(err)
        }
    }
}

/// We don't have a fully deterministic way to tell if our command failed
/// because of the sandbox - a command in the user's zshrc file might hit an
/// error, but the command itself might fail or succeed for other reasons.
/// For now, we conservatively check for 'command not found' (exit code 127),
/// and can add additional cases as necessary.
fn is_likely_sandbox_denied(sandbox_type: SandboxType, exit_code: i32) -> bool {
    if sandbox_type == SandboxType::None {
        return false;
    }

    // Quick rejects: well-known non-sandbox shell exit codes
    // 127: command not found, 2: misuse of shell builtins
    if exit_code == 127 {
        return false;
    }

    // For all other cases, we assume the sandbox is the cause
    true
}

#[derive(Debug)]
pub struct StreamOutput<T> {
    pub text: T,
    pub truncated_after_lines: Option<u32>,
    pub truncated_by_bytes: bool,
}
#[derive(Debug)]
struct RawExecToolCallOutput {
    pub exit_status: ExitStatus,
    pub stdout: StreamOutput<Vec<u8>>,
    pub stderr: StreamOutput<Vec<u8>>,
    pub aggregated_output: StreamOutput<Vec<u8>>,
    pub timed_out: bool,
}

impl StreamOutput<String> {
    pub fn new(text: String) -> Self {
        Self {
            text,
            truncated_after_lines: None,
            truncated_by_bytes: false,
        }
    }
}

impl StreamOutput<Vec<u8>> {
    pub fn from_utf8_lossy(&self) -> StreamOutput<String> {
        StreamOutput {
            text: String::from_utf8_lossy(&self.text).to_string(),
            truncated_after_lines: self.truncated_after_lines,
            truncated_by_bytes: self.truncated_by_bytes,
        }
    }
}

#[inline]
fn append_all(dst: &mut Vec<u8>, src: &[u8]) {
    dst.extend_from_slice(src);
}

#[derive(Debug)]
pub struct ExecToolCallOutput {
    pub exit_code: i32,
    pub stdout: StreamOutput<String>,
    pub stderr: StreamOutput<String>,
    pub aggregated_output: StreamOutput<String>,
    pub duration: Duration,
    pub timed_out: bool,
}

async fn exec(
    params: ExecParams,
    sandbox_policy: &SandboxPolicy,
    stdout_stream: Option<StdoutStream>,
    output_limit: ExecOutputLimit,
) -> Result<RawExecToolCallOutput> {
    let timeout = params.timeout_duration();
    let ExecParams {
        command, cwd, env, ..
    } = params;

    let (program, args) = command.split_first().ok_or_else(|| {
        CodexErr::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command args are empty",
        ))
    })?;
    let arg0 = None;
    let child = spawn_child_async(
        PathBuf::from(program),
        args.into(),
        arg0,
        cwd,
        sandbox_policy,
        StdioPolicy::RedirectForShellTool,
        env,
    )
    .await?;
    consume_truncated_output(child, timeout, stdout_stream, output_limit).await
}

/// Consumes the output of a child process, truncating it so it is suitable for
/// use as the output of a `shell` tool call. Also enforces specified timeout.
async fn consume_truncated_output(
    mut child: Child,
    timeout: Duration,
    stdout_stream: Option<StdoutStream>,
    output_limit: ExecOutputLimit,
) -> Result<RawExecToolCallOutput> {
    // Both stdout and stderr were configured with `Stdio::piped()`
    // above, therefore `take()` should normally return `Some`.  If it doesn't
    // we treat it as an exceptional I/O error

    let stdout_reader = child.stdout.take().ok_or_else(|| {
        CodexErr::Io(io::Error::other(
            "stdout pipe was unexpectedly not available",
        ))
    })?;
    let stderr_reader = child.stderr.take().ok_or_else(|| {
        CodexErr::Io(io::Error::other(
            "stderr pipe was unexpectedly not available",
        ))
    })?;

    let (agg_tx, agg_rx) = async_channel::unbounded::<Vec<u8>>();

    let stdout_handle = tokio::spawn(read_capped(
        BufReader::new(stdout_reader),
        stdout_stream.clone(),
        false,
        Some(agg_tx.clone()),
        output_limit.stream_max_bytes,
    ));
    let stderr_handle = tokio::spawn(read_capped(
        BufReader::new(stderr_reader),
        stdout_stream.clone(),
        true,
        Some(agg_tx.clone()),
        output_limit.stream_max_bytes,
    ));

    let (exit_status, timed_out) = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => {
            match result {
                Ok(status_result) => {
                    let exit_status = status_result?;
                    (exit_status, false)
                }
                Err(_) => {
                    // timeout
                    child.start_kill()?;
                    // Debatable whether `child.wait().await` should be called here.
                    (synthetic_exit_status(EXIT_CODE_SIGNAL_BASE + TIMEOUT_CODE), true)
                }
            }
        }
        _ = tokio::signal::ctrl_c() => {
            child.start_kill()?;
            (synthetic_exit_status(EXIT_CODE_SIGNAL_BASE + SIGKILL_CODE), false)
        }
    };

    let stdout = stdout_handle.await??;
    let stderr = stderr_handle.await??;

    drop(agg_tx);

    let mut combined_buf = Vec::with_capacity(
        output_limit
            .aggregated_max_bytes
            .min(AGGREGATE_BUFFER_INITIAL_CAPACITY),
    );
    let mut aggregated_truncated = false;
    while let Ok(chunk) = agg_rx.recv().await {
        if combined_buf.len() < output_limit.aggregated_max_bytes {
            let remaining = output_limit
                .aggregated_max_bytes
                .saturating_sub(combined_buf.len());
            let take = remaining.min(chunk.len());
            if take > 0 {
                append_all(&mut combined_buf, &chunk[..take]);
            }
            if take < chunk.len() {
                aggregated_truncated = true;
            }
        } else {
            aggregated_truncated = true;
        }
    }
    let aggregated_output = StreamOutput {
        text: combined_buf,
        truncated_after_lines: None,
        truncated_by_bytes: aggregated_truncated,
    };

    Ok(RawExecToolCallOutput {
        exit_status,
        stdout,
        stderr,
        aggregated_output,
        timed_out,
    })
}

async fn read_capped<R: AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    stream: Option<StdoutStream>,
    is_stderr: bool,
    aggregate_tx: Option<Sender<Vec<u8>>>,
    max_bytes: usize,
) -> io::Result<StreamOutput<Vec<u8>>> {
    let mut buf = Vec::with_capacity(AGGREGATE_BUFFER_INITIAL_CAPACITY);
    let mut tmp = [0u8; READ_CHUNK_SIZE];
    let mut emitted_deltas: usize = 0;
    let mut truncated = false;

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break;
        }

        if let Some(stream) = &stream
            && emitted_deltas < MAX_EXEC_OUTPUT_DELTAS_PER_CALL
        {
            let chunk = tmp[..n].to_vec();
            let msg = EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
                call_id: stream.call_id.clone(),
                stream: if is_stderr {
                    ExecOutputStream::Stderr
                } else {
                    ExecOutputStream::Stdout
                },
                chunk,
            });
            let event = Event {
                id: stream.sub_id.clone(),
                msg,
            };
            #[allow(clippy::let_unit_value)]
            let _ = stream.tx_event.send(event).await;
            emitted_deltas += 1;
        }

        if let Some(tx) = &aggregate_tx {
            let _ = tx.send(tmp[..n].to_vec()).await;
        }

        if buf.len() < max_bytes {
            let remaining = max_bytes.saturating_sub(buf.len());
            let take = remaining.min(n);
            if take > 0 {
                append_all(&mut buf, &tmp[..take]);
            }
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
        // Continue reading to EOF to avoid back-pressure
    }

    Ok(StreamOutput {
        text: buf,
        truncated_after_lines: None,
        truncated_by_bytes: truncated,
    })
}

fn append_truncation_notice(output: &mut StreamOutput<String>, notice: &str) {
    if !output.truncated_by_bytes {
        return;
    }

    if !output.text.is_empty() && !output.text.ends_with('\n') {
        output.text.push('\n');
    }
    output.text.push_str(notice);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ripgrep_plain_command() {
        let command = vec!["rg".to_string(), "needle".to_string()];
        let limits = exec_output_limit_for_command(&command);
        assert_eq!(limits.aggregated_max_bytes, RG_EXEC_OUTPUT_MAX_BYTES);
        assert_eq!(limits.stream_max_bytes, RG_EXEC_OUTPUT_MAX_BYTES);
        assert_eq!(limits.truncation_notice, RG_EXEC_TRUNCATION_NOTICE);
    }

    #[test]
    fn detects_ripgrep_via_bash() {
        let command = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "rg --json term".to_string(),
        ];
        let limits = exec_output_limit_for_command(&command);
        assert_eq!(limits.aggregated_max_bytes, RG_EXEC_OUTPUT_MAX_BYTES);
    }

    #[test]
    fn defaults_to_generic_for_other_commands() {
        let command = vec!["python".to_string(), "script.py".to_string()];
        let limits = exec_output_limit_for_command(&command);
        assert_eq!(limits.aggregated_max_bytes, GENERIC_EXEC_OUTPUT_MAX_BYTES);
        assert_eq!(limits.truncation_notice, GENERIC_EXEC_TRUNCATION_NOTICE);
    }
}

#[cfg(unix)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code)
}

#[cfg(windows)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    #[expect(clippy::unwrap_used)]
    std::process::ExitStatus::from_raw(code.try_into().unwrap())
}
