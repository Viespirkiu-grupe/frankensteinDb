use std::fs::File;
use std::io::Read;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::*;

impl Database {
    /// Creates a portable zstd-compressed archive after publishing all pending writes.
    /// Writes must remain externally serialized for the duration of this call; Tantivy reads may
    /// continue from their existing snapshots.
    pub fn backup_to(&mut self, destination: impl AsRef<Path>) -> Result<QueryResult> {
        self.flush()?;
        for handle in self.indexes.values_mut() {
            if let Some(writer) = handle.writer.take() {
                writer.wait_merging_threads()?;
            }
        }
        self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;

        let file = File::create(destination.as_ref())?;
        let encoder = zstd::Encoder::new(file, 3)?;
        let mut archive = tar::Builder::new(encoder);
        let mut manifest = Vec::new();
        append_file(
            &mut archive,
            &self.root.join("data.sqlite3"),
            Path::new("data.sqlite3"),
            &mut manifest,
        )?;
        append_directory(
            &mut archive,
            &self.root.join("indexes"),
            Path::new("indexes"),
            &mut manifest,
        )?;
        let document = serde_json::to_vec_pretty(&json!({
            "format": 1,
            "created_at": Utc::now().to_rfc3339(),
            "files": manifest,
        }))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(document.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, "manifest.json", document.as_slice())?;
        archive.into_inner()?.finish()?;
        Ok(QueryResult::message("backup created"))
    }
}

/// Restores a portable backup into an offline database directory.
pub fn restore_backup(
    root: impl AsRef<Path>,
    archive: impl AsRef<Path>,
    force: bool,
) -> Result<()> {
    let root = root.as_ref();
    ensure!(force, "restore requires --force");
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("database");
    let temporary = parent.join(format!(".{name}.restoring"));
    ensure!(
        !temporary.exists(),
        "restore staging path already exists: {}",
        temporary.display()
    );
    fs::create_dir_all(&temporary)?;
    let result = (|| -> Result<()> {
        let decoder = zstd::Decoder::new(File::open(archive.as_ref())?)?;
        tar::Archive::new(decoder).unpack(&temporary)?;
        verify_manifest(&temporary)?;
        let database = Database::open(&temporary).context("restored database validation failed")?;
        drop(database);
        let previous = parent.join(format!(".{name}.before-restore"));
        ensure!(
            !previous.exists(),
            "previous restore directory already exists: {}",
            previous.display()
        );
        if root.exists() {
            fs::rename(root, &previous)?;
        }
        if let Err(error) = fs::rename(&temporary, root) {
            if previous.exists() {
                let _ = fs::rename(&previous, root);
            }
            return Err(error.into());
        }
        if previous.exists() {
            fs::remove_dir_all(previous)?;
        }
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

#[derive(Deserialize)]
struct BackupManifest {
    format: u32,
    files: Vec<BackupFile>,
}

#[derive(Deserialize)]
struct BackupFile {
    path: String,
    bytes: u64,
    sha256: String,
}

fn verify_manifest(root: &Path) -> Result<()> {
    let manifest: BackupManifest = serde_json::from_slice(
        &fs::read(root.join("manifest.json")).context("backup manifest is missing")?,
    )?;
    ensure!(
        manifest.format == 1,
        "unsupported backup format: {}",
        manifest.format
    );
    for entry in manifest.files {
        let path = root.join(&entry.path);
        ensure!(path.starts_with(root), "unsafe backup path: {}", entry.path);
        ensure!(
            fs::metadata(&path)?.len() == entry.bytes,
            "backup size mismatch: {}",
            entry.path
        );
        let mut file = File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        ensure!(
            hex::encode(hasher.finalize()) == entry.sha256,
            "backup checksum mismatch: {}",
            entry.path
        );
    }
    Ok(())
}

fn append_directory(
    archive: &mut tar::Builder<zstd::Encoder<'static, File>>,
    source: &Path,
    archive_root: &Path,
    manifest: &mut Vec<Value>,
) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let archive_path = archive_root.join(entry.file_name());
        if path.is_dir() {
            append_directory(archive, &path, &archive_path, manifest)?;
        } else {
            append_file(archive, &path, &archive_path, manifest)?;
        }
    }
    Ok(())
}

fn append_file(
    archive: &mut tar::Builder<zstd::Encoder<'static, File>>,
    source: &Path,
    archive_path: &Path,
    manifest: &mut Vec<Value>,
) -> Result<()> {
    let mut source_file = File::open(source)?;
    let bytes = source_file.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source_file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    manifest.push(json!({
        "path": archive_path.to_string_lossy(),
        "bytes": bytes,
        "sha256": hex::encode(hasher.finalize()),
    }));
    let mut source_file = File::open(source)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, archive_path, &mut source_file)?;
    Ok(())
}
