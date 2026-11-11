-- I-IDE's own extension store: each published version of an open-source
-- extension, built from its source, with the package the IDE downloads.
CREATE TABLE IF NOT EXISTS extension_versions (
    id TEXT NOT NULL,
    version TEXT NOT NULL,
    -- The package's manifest as the IDE reads it (name, description,
    -- authors, repository, schema and wasm API versions, what it provides).
    manifest TEXT NOT NULL,
    archive BLOB NOT NULL,
    published_at TEXT NOT NULL,
    download_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (id, version)
);
