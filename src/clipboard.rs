use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

pub fn copy_text(text: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return copy_with_stdin("pbcopy", &[], text);
    }

    #[cfg(target_os = "windows")]
    {
        return copy_with_stdin("clip", &[], text);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let linux_commands: [(&str, &[&str]); 3] = [
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];

        let mut last_error = None;
        for (cmd, args) in linux_commands {
            match copy_with_stdin(cmd, args, text) {
                Ok(_) => return Ok(()),
                Err(e) => last_error = Some(e),
            }
        }

        let err = last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "No clipboard backend available".to_string());
        Err(anyhow!(err))
    }
}

fn copy_with_stdin(cmd: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to start {}", cmd))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .with_context(|| format!("Failed to write to {}", cmd))?;
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("Failed while waiting for {}", cmd))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(anyhow!("{} failed: {}", cmd, stderr))
    }
}
