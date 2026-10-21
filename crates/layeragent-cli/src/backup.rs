use anyhow::Result;
use clap::Subcommand;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum BackupCommand {
    /// Generate a private age identity. Store it outside your runtime home.
    Keygen {
        #[arg(long)]
        identity: PathBuf,
    },
    /// Snapshot the stopped runtime into an authenticated age-encrypted archive.
    Create {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        recipient: String,
    },
    /// Decrypt and verify every checksum without restoring data.
    Verify {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        identity: PathBuf,
    },
    /// Restore into a new, nonexistent home; reconnect credentials afterwards.
    Restore {
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        destination: PathBuf,
    },
    /// Delete older archives matching layeragent-backup-<20 digit timestamp>.age.
    Retain {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        keep: usize,
    },
}
pub fn dispatch(home: &Path, command: BackupCommand) -> Result<()> {
    use layeragent_storage::backup;
    match command {
        BackupCommand::Keygen { identity } => {
            if let Ok(home) = home.canonicalize() {
                let parent = identity
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                anyhow::ensure!(
                    !parent.canonicalize()?.starts_with(home),
                    "identity must be outside the runtime home"
                );
            }
            println!("{}", backup::keygen(&identity)?);
            return Ok(());
        }
        BackupCommand::Retain { directory, keep } => {
            println!(
                "Removed {} old encrypted archives",
                backup::retain(&directory, keep)?
            );
            return Ok(());
        }
        command => {
            let manifest = match command {
                BackupCommand::Create { output, recipient } => {
                    backup::create(home, &output, &recipient)?
                }
                BackupCommand::Verify { archive, identity } => backup::verify(&archive, &identity)?,
                BackupCommand::Restore {
                    archive,
                    identity,
                    destination,
                } => backup::restore(&archive, &identity, &destination)?,
                _ => unreachable!(),
            };
            // Manifest reveals names and hashes only after explicit verification;
            // configuration and file contents are never printed.
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
    }
    Ok(())
}
