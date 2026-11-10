/// Suppresses the console window Windows otherwise flashes open for every
/// spawned child process — Java version checks, GPU detection, pre/post
/// launch shell commands, and the actual game launch itself all spawn a
/// subprocess, and without this a GUI app doing that gets a briefly (or,
/// for the long-lived Minecraft process, permanently) visible black
/// console window alongside it. No-op on other platforms, where this
/// isn't a thing.
#[cfg(windows)]
pub fn suppress_console_window(cmd: &mut tokio::process::Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn suppress_console_window(_cmd: &mut tokio::process::Command) {}
