# slam

slam a lot of changes into a large number of repos in one fell swoop

## Logging and Debugging

SLAM uses the `env_logger` crate for logging. You can control the log level using the `RUST_LOG` environment variable:

### Log Levels

- `error` - Only errors
- `warn` - Warnings and errors
- `info` - Informational messages, warnings, and errors
- `debug` - Detailed debug information (recommended for troubleshooting)
- `trace` - Very verbose output

### Examples

```bash
# Enable debug logging for all modules
RUST_LOG=debug slam review purge

# Enable debug logging only for slam
RUST_LOG=slam=debug slam review purge

# Enable info level logging
RUST_LOG=info slam review purge

# Enable debug logging for specific operations
RUST_LOG=debug slam review purge 2>&1 | tee slam-debug.log
```

### Troubleshooting Common Issues

#### JSON Parsing Errors

If you see errors like "Failed to parse open PRs JSON", enable debug logging to see the raw GitHub CLI output:

```bash
RUST_LOG=debug slam review purge
```

This will show:
- The exact GitHub CLI commands being executed
- The raw JSON responses from GitHub
- Detailed parsing information

#### GitHub CLI Issues

SLAM relies on the GitHub CLI (`gh`) being properly configured. Ensure:

1. `gh` is installed and in your PATH
2. You're authenticated: `gh auth status`
3. You have appropriate permissions for the repositories

#### Repository Access Issues

If you see "Failed to list open PRs" or "Failed to list remote branches" errors:

1. Verify you have access to the repository
2. Check that the repository exists
3. Ensure your GitHub token has the necessary scopes

### Example Debug Session

```bash
# Run with full debug logging
RUST_LOG=debug slam review purge -o tatari-tv

# Save debug output to file
RUST_LOG=debug slam review purge -o tatari-tv 2>&1 | tee debug.log

# Filter debug output for specific repo
RUST_LOG=debug slam review purge -o tatari-tv 2>&1 | grep "philo-fe"
```
