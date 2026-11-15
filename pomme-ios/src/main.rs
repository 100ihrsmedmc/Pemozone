//! iOS entry point: the app's executable, which reads the launch arguments
//! from the app's Documents folder and runs the client, with its game data in
//! a Documents subfolder the Files app can reach.

#[cfg(target_os = "ios")]
fn main() {
    use pomme_client::{LAUNCH_FILE, LaunchArgs, Platform};

    // An app's HOME is its container.
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let documents = home.join("Documents");

    // iOS creates an app's Documents folder lazily and only lists the app in
    // the Files app once that folder exists, so a client that never writes
    // there can be missing from Files entirely.
    if let Err(error) = std::fs::create_dir_all(&documents) {
        eprintln!("Couldn't create {}: {error}", documents.display());
        std::process::exit(1);
    }
    let readme = documents.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(
            &readme,
            "Pemozone's documents folder.\n\n\
             Drop a launch.txt here to pass the same flags the desktop client\n\
             takes; see pomme-ios/README.md for the flags. Game data lives in\n\
             the 'Game Data' folder next to this file.\n",
        );
    }

    // The client's default root, in Application Support, is invisible in the
    // Files app, so game data goes next to launch.txt instead.
    let visible_data = documents.join("Game Data");
    let old_data = home
        .join("Library")
        .join("Application Support")
        .join("Pemozone");
    if let Err(error) = migrate_contents(&old_data, &visible_data) {
        // Splitting the game data across two folders would hide half of it
        // from the client, so stop instead.
        eprintln!(
            "Couldn't move {} to {}: {error}",
            old_data.display(),
            visible_data.display()
        );
        std::process::exit(1);
    }
    // Set it before the client touches its data folders, so `layout` is built
    // on the visible root.
    pomme_dirs::set_root(visible_data.clone());

    pomme_client::run(
        LaunchArgs::from_file(&documents.join(LAUNCH_FILE)),
        Platform {
            data_root: Some(visible_data),
        },
    );
}

/// Moves everything in `from` into `to`, merging folders and leaving any file
/// whose name already exists in `to` where it is, so a later launch can retry
/// it. Both folders are removed once empty.
#[cfg(target_os = "ios")]
fn migrate_contents(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    use std::fs;
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if destination.exists() {
            if source.is_dir() && destination.is_dir() {
                migrate_contents(&source, &destination)?;
                if fs::read_dir(&source)?.next().is_none() {
                    fs::remove_dir(&source)?;
                }
            }
            // A conflicting file or directory is preserved.
            continue;
        }
        pomme_dirs::move_path(&source, &destination)?;
    }
    if fs::read_dir(from)?.next().is_none() {
        fs::remove_dir(from)?;
    }
    Ok(())
}

/// Built along with the rest of the workspace, where it has nothing to run.
#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("pomme-ios only runs on iOS; see pomme-ios/README.md");
}
