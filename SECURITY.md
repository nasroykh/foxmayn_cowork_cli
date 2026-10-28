# Security Policy

## Reporting a vulnerability

Use GitHub private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**. Do not file public issues for security problems. Expect an initial response within a few days.

## Supported versions

| Version | Supported |
|---------|-----------|
| 1.x     | Yes       |
| < 1.0   | No        |

## Threat model

- The LLM can only act inside the chosen working directory; paths that escape it are rejected.
- Destructive operations (edit, patch, delete, rename, bulk variants) require explicit user confirmation unless `--skip-confirmations` is set for the session.
- Writes to hidden paths also require confirmation.
- The API key is stored in `~/.config/foxmayn-cowork/.env` with file mode 0600.
- File contents and conversations are sent to the configured LLM provider and saved locally in the app's SQLite storage; use the `local` or `ollama` provider to keep data on your machine.
