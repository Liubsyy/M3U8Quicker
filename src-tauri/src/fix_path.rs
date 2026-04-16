// Based on https://github.com/tauri-apps/fix-path-env-rs
// Copyright 2021 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Reads the user's shell configuration to properly set the PATH environment variable.
///
/// On macOS/Linux, GUI apps don't inherit PATH from shell dotfiles (.zshrc, .bashrc, etc.).
/// This function launches a login shell to read the real PATH and applies it to the current process.
///
/// On Windows, this is a no-op.
pub fn fix() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        if let Ok(value) = std::env::var("PATH") {
            std::env::set_var("PATH", value);
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let default_shell = if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/sh"
        };
        let shell = std::env::var("SHELL").unwrap_or_else(|_| default_shell.into());

        let mut cmd = std::process::Command::new(shell);
        cmd.arg("-ilc")
            .arg("echo -n \"_SHELL_ENV_DELIMITER_\"; env; echo -n \"_SHELL_ENV_DELIMITER_\"; exit")
            .env("DISABLE_AUTO_UPDATE", "true");

        if let Some(home) = dirs::home_dir() {
            cmd.current_dir(home);
        }

        let out = cmd.output()?;

        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        let env = stdout
            .split("_SHELL_ENV_DELIMITER_")
            .nth(1)
            .ok_or("invalid output from shell")?;

        for line in String::from_utf8_lossy(&strip_ansi_escapes::strip(env))
            .split('\n')
            .filter(|l| !l.is_empty())
        {
            let mut s = line.splitn(2, '=');
            if let (Some(var), Some(value)) = (s.next(), s.next()) {
                if var == "PATH" {
                    std::env::set_var("PATH", value);
                    break;
                }
            }
        }

        Ok(())
    }
}
