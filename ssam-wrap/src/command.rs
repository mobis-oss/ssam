// Copyright 2025-2026 Hyundai Mobis Co., Ltd.
// SPDX-License-Identifier: Apache-2.0

//! Abstraction over external command execution so orchestration logic across
//! the crate can be unit-tested with a mock in place of real process spawning.

use anyhow::Context;
use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};

/// Whether a child process's stdout/stderr are captured (piped) or inherited
/// from the parent. `true` pipes the stream so it can be read back; `false`
/// lets it flow to the parent's terminal.
#[derive(Debug, Clone, Copy)]
pub struct OutputCapture {
    pub stdout: bool,
    pub stderr: bool,
}

impl OutputCapture {
    fn as_stdio(capture: bool) -> Stdio {
        if capture {
            Stdio::piped()
        } else {
            Stdio::inherit()
        }
    }
}

/// Abstraction over spawning external commands so orchestration logic can be
/// unit-tested with a mock in place of real process execution.
///
/// Requires `Debug` so that holders (e.g. `Workspace`) can derive `Debug`.
pub trait CommandRunner: std::fmt::Debug {
    /// Low-level primitive: spawn `cmd` with `args` and return the raw output.
    /// This is the only method a mock needs to override.
    fn run(&self, cmd: &str, args: &[&OsStr], capture: OutputCapture) -> anyhow::Result<Output>;

    /// Ergonomic wrapper over [`CommandRunner::run`]: runs `cmd` with `args`,
    /// bails on a non-zero exit status, and returns stdout as a `String`.
    ///
    /// Non-generic (`args: &[&str]`) on purpose so the trait stays
    /// dyn-compatible and this method is callable on `&dyn CommandRunner`.
    fn execute_command(
        &self,
        cmd: &str,
        args: &[&str],
        capture_stdout: bool,
    ) -> anyhow::Result<String> {
        let arg_refs: Vec<&OsStr> = args.iter().map(OsStr::new).collect();

        let output = self.run(
            cmd,
            &arg_refs,
            OutputCapture {
                stdout: capture_stdout,
                stderr: false,
            },
        )?;

        if !output.status.success() {
            // stderr is inherited (not captured), so output.stderr is empty here;
            // kept for parity with the previous behavior.
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "Command {cmd:?} {arg_refs:?} failed with status {}\nStderr: {}",
                output.status,
                stderr
            );
        }

        if !capture_stdout || output.stdout.is_empty() {
            Ok(String::new())
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(stdout)
        }
    }
}

/// Real runner backed by [`std::process::Command`].
#[derive(Debug)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, cmd: &str, args: &[&OsStr], capture: OutputCapture) -> anyhow::Result<Output> {
        let mut command = Command::new(cmd);
        command.args(args);
        command.stdout(OutputCapture::as_stdio(capture.stdout));
        command.stderr(OutputCapture::as_stdio(capture.stderr));
        command.output().context(format!("Failed to run {cmd}"))
    }
}

/// Test helpers shared across the crate's unit tests. Exposed as `pub(crate)`
/// so modules (`pkgfs`, `veritysetup`, ...) can inject a mock runner in place
/// of real process execution.
#[cfg(test)]
#[allow(dead_code)] // parts of the mock API are consumed by later test modules
pub(crate) mod testing {
    use super::{CommandRunner, OutputCapture};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};
    use std::rc::Rc;

    /// One recorded invocation of [`CommandRunner::run`].
    #[derive(Debug, Clone)]
    pub struct RecordedCall {
        pub cmd: String,
        pub args: Vec<OsString>,
        pub capture: OutputCapture,
    }

    impl RecordedCall {
        /// Args as `&str` for convenient assertions in tests.
        pub fn arg_strs(&self) -> Vec<String> {
            self.args
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        }
    }

    /// A programmed response for a single `run` invocation.
    #[derive(Debug)]
    pub struct MockResponse {
        pub code: i32,
        pub stdout: Vec<u8>,
        pub stderr: Vec<u8>,
    }

    impl MockResponse {
        pub fn ok() -> Self {
            Self {
                code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        }

        pub fn stdout(s: &str) -> Self {
            Self {
                code: 0,
                stdout: s.as_bytes().to_vec(),
                stderr: Vec::new(),
            }
        }

        pub fn failure(code: i32, stderr: &str) -> Self {
            Self {
                code,
                stdout: Vec::new(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }

        fn exit_status(&self) -> ExitStatus {
            // On Unix, `from_raw` takes a wait status. Encode a plain exit code:
            // 0 -> success, N -> non-zero exit (N in the high byte).
            let raw = if self.code == 0 {
                0
            } else {
                (self.code & 0xff) << 8
            };
            ExitStatus::from_raw(raw)
        }
    }

    #[derive(Debug, Default)]
    struct MockState {
        calls: Vec<RecordedCall>,
        responses: VecDeque<MockResponse>,
    }

    /// Records every call and returns queued responses (defaulting to success
    /// once the queue is drained).
    ///
    /// The recording state lives behind an `Rc<RefCell<..>>`, so a `clone()` is
    /// a cheap shared handle to the *same* state. This lets a test keep a clone
    /// to inspect calls after moving another clone into a `Workspace` (which
    /// owns its runner as `Box<dyn CommandRunner>`).
    #[derive(Debug, Clone, Default)]
    pub struct MockCommandRunner {
        state: Rc<RefCell<MockState>>,
    }

    impl MockCommandRunner {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_responses(responses: Vec<MockResponse>) -> Self {
            Self {
                state: Rc::new(RefCell::new(MockState {
                    calls: Vec::new(),
                    responses: responses.into(),
                })),
            }
        }

        pub fn push_response(&self, response: MockResponse) {
            self.state.borrow_mut().responses.push_back(response);
        }

        pub fn calls(&self) -> Vec<RecordedCall> {
            self.state.borrow().calls.clone()
        }
    }

    impl CommandRunner for MockCommandRunner {
        fn run(
            &self,
            cmd: &str,
            args: &[&OsStr],
            capture: OutputCapture,
        ) -> anyhow::Result<Output> {
            let mut state = self.state.borrow_mut();
            state.calls.push(RecordedCall {
                cmd: cmd.to_string(),
                args: args.iter().map(|a| a.to_os_string()).collect(),
                capture,
            });

            let response = state.responses.pop_front().unwrap_or_else(MockResponse::ok);

            Ok(Output {
                status: response.exit_status(),
                stdout: response.stdout,
                stderr: response.stderr,
            })
        }
    }

    #[test]
    fn records_calls_and_returns_queued_responses() {
        let runner = MockCommandRunner::with_responses(vec![
            MockResponse::stdout("hello"),
            MockResponse::failure(2, "boom"),
        ]);
        // A clone shares the same recording state, mimicking how a test keeps a
        // handle after the runner is moved into a Workspace.
        let handle = runner.clone();

        let out = runner
            .run(
                "echo",
                &[OsStr::new("a"), OsStr::new("b")],
                OutputCapture {
                    stdout: true,
                    stderr: false,
                },
            )
            .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hello");

        let out = runner
            .run(
                "false",
                &[],
                OutputCapture {
                    stdout: false,
                    stderr: true,
                },
            )
            .unwrap();
        assert!(!out.status.success());
        assert_eq!(out.status.code(), Some(2));

        // Queue drained -> defaults to success.
        let out = runner
            .run(
                "true",
                &[],
                OutputCapture {
                    stdout: false,
                    stderr: false,
                },
            )
            .unwrap();
        assert!(out.status.success());

        // Inspecting through the shared clone sees every call.
        let calls = handle.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].cmd, "echo");
        assert_eq!(calls[0].arg_strs(), vec!["a", "b"]);
        assert_eq!(calls[1].cmd, "false");
    }
}
