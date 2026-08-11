# Sandbox — OS-level execution sandboxing

Restricts subprocess execution via platform-specific sandboxes:
macOS Seatbelt (`sandbox-exec`), Linux bubblewrap (`bwrap`), and Windows
JobSandbox (Job Object: process-tree isolation + active-process/memory
limits; network and filesystem-write policies are not enforced on Windows).

## License

Licensed under the same terms as deepseeknova.
