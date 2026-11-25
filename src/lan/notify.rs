//! Desktop notifications for pairing: the printer's display. Failures are
//! logged; the code is also shown by `session-bridge pair` and the admin page.
use std::process::Stdio;
use tokio::process::Command;

/// Shows a notification when `enabled`; waits for the helper and logs failures.
pub async fn show(enabled: bool, title: &str, body: &str) {
    if let Some(command) = command(title, body).filter(|_| enabled) {
        run(command).await;
    }
}
async fn run(mut command: Command) {
    let result = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await;
    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => eprintln!(
            "Desktop notification failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(e) => eprintln!("Desktop notification failed: {e}"),
    }
}

/// Quotes `value` for an AppleScript or PowerShell string literal.
fn quoted(value: &str, quote: char) -> String {
    let mut out = String::from(quote);
    for c in value.chars().filter(|c| !c.is_control()) {
        if c == quote || (quote == '"' && c == '\\') {
            out.push(if quote == '"' { '\\' } else { quote });
        }
        out.push(c);
    }
    out.push(quote);
    out
}

#[cfg(target_os = "macos")]
fn command(title: &str, body: &str) -> Option<Command> {
    let mut command = Command::new("/usr/bin/osascript");
    command.arg("-e").arg(format!(
        "display notification {} with title {}",
        quoted(body, '"'),
        quoted(title, '"')
    ));
    Some(command)
}

#[cfg(windows)]
fn command(title: &str, body: &str) -> Option<Command> {
    // A toast through the Windows Runtime, attributed to PowerShell.
    let script = format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
         $t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
         $x = $t.GetElementsByTagName('text'); \
         $x.Item(0).AppendChild($t.CreateTextNode({})) | Out-Null; \
         $x.Item(1).AppendChild($t.CreateTextNode({})) | Out-Null; \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show([Windows.UI.Notifications.ToastNotification]::new($t))",
        quoted(title, '\''),
        quoted(body, '\'')
    );
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    Some(command)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn command(_title: &str, _body: &str) -> Option<Command> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /* REQ-017: Notification text cannot break out of its script string. */
    #[test]
    fn quotes_script_strings() {
        assert_eq!(quoted(r#"a "b" \c"#, '"'), r#""a \"b\" \\c""#);
        assert_eq!(quoted("it's", '\''), "'it''s'");
        assert_eq!(quoted("line\nbreak", '"'), "\"linebreak\"");
    }

    /* REQ-017: A disabled notifier does nothing; helper failures are logged, not raised. */
    #[tokio::test]
    async fn runs_helpers_and_survives_failures() {
        show(false, "title", "body").await;
        run(Command::new("true")).await;
        run(Command::new("false")).await;
        run(Command::new("/nonexistent/notifier")).await;
    }

    /* REQ-017: The macOS notifier passes the quoted text to osascript. */
    #[cfg(target_os = "macos")]
    #[test]
    fn builds_the_macos_notifier() {
        let command = command("Pairing", "laptop-a wants to pair. Code 482-913").unwrap();
        let args: Vec<_> = command.as_std().get_args().collect();
        assert_eq!(command.as_std().get_program(), "/usr/bin/osascript");
        assert_eq!(
            args[1],
            r#"display notification "laptop-a wants to pair. Code 482-913" with title "Pairing""#
        );
    }
}
