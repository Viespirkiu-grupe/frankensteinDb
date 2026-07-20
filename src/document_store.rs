use super::*;

/// Converts the durable table configuration into Tantivy generation settings.
pub(crate) fn index_settings(def: &TableDef) -> IndexSettings {
    let compression = match def.document_store.compression {
        DocumentCompression::Lz4 => Compressor::Lz4,
        DocumentCompression::Zstd => Compressor::Zstd(ZstdCompressor {
            compression_level: def.document_store.zstd_level,
        }),
        DocumentCompression::None => Compressor::None,
    };
    IndexSettings {
        docstore_compression: compression,
        docstore_blocksize: def.document_store.block_size,
        docstore_compress_dedicated_thread: def.document_store.dedicated_thread,
    }
}

/// Rejects settings that could cause pathological allocation or unsupported Zstd work.
pub(crate) fn validate_document_store(settings: &DocumentStore) -> Result<()> {
    ensure!(
        (1_024..=16 * 1024 * 1024).contains(&settings.block_size),
        "document-store block_size must be between 1024 and 16777216 bytes"
    );
    ensure!(
        settings.compression == DocumentCompression::Zstd || settings.zstd_level.is_none(),
        "zstd_level is only valid with zstd compression"
    );
    if let Some(level) = settings.zstd_level {
        ensure!(
            zstd::compression_level_range().contains(&level),
            "zstd_level is outside the supported range"
        );
    }
    Ok(())
}
