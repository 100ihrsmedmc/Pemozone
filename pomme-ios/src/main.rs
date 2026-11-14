//! iOS entry point: the app's executable, which reads the launch arguments
//! from the app's Documents folder and runs the client, with its data in the
//! app's Application Support.

#[cfg(target_os = "ios")]
fn main() {
    use pomme_client::{LAUNCH_FILE, LaunchArgs, Platform};

    // An app's HOME is its container.
    let documents = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join("Documents");
    // iOS creates an app's Documents folder lazily and only lists the app in
    // the Files app once that folder exists, so a client that never writes
    // there can be missing from Files entirely. Create it and leave a note,
    // since an empty folder may be listed as empty or skipped.
    let _ = std::fs::create_dir_all(&documents);
    let readme = documents.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(
            &readme,
            "Pemozone's documents folder.\n\n\
             Drop a launch.txt here to pass the same flags the desktop client\n\
             takes; see pomme-ios/README.md for the flags.\n",
        );
    }
    pomme_client::run(
        LaunchArgs::from_file(&documents.join(LAUNCH_FILE)),
        Platform { data_root: None },
    );
}

/// Built along with the rest of the workspace, where it has nothing to run.
#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("pomme-ios only runs on iOS; see pomme-ios/README.md");
}
