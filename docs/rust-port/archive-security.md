# Archive Extraction Security (Rust Port)

This bot accepts user-supplied archives (zip/tar/tar.gz). Extraction must be treated as hostile input.

## Implementation

`ctb-core::archive_security` provides `safe_extract_archive()` which enforces:
- No path traversal: rejects `..`, absolute paths, Windows drive prefixes.
- No symlinks/hardlinks/devices: only regular files + directories are allowed.
- Resource limits: max files, max bytes per file, max total bytes extracted.

## Intended Use In Document Handler

1. Create a fresh extraction directory under the bot temp dir (ex: `${TEMP_DIR}/archive_<timestamp>`).
2. Call `safe_extract_archive(archive_path, original_filename, extract_dir, limits)`.
3. Build a file tree and read only allowed text extensions with additional *text* limits (chars/file, total chars).
4. Always delete the extraction directory after processing (best-effort).

Notes:
- Prefer reading files after extraction using the same allowlist as TS (`.md`, `.txt`, `.json`, ...), and cap output size so the prompt stays bounded.
- If extraction fails, report a short error to the user and do not continue.

