//! What a packed `.mcpb` file holds, read back from it.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::{Error, MANIFEST_VERSION};

/// What ends a signed bundle, after the zip and the signature. This crate does
/// not sign, so it only reports whether a bundle carries a signature.
const SIGNATURE_END: &[u8] = b"MCPB_SIG_END";

/// A `.mcpb` file, read back: its manifest, what it holds, and whether it is
/// signed.
///
/// ```no_run
/// let bundle = mcpb::Bundle::open("dist/my-server-1.2.3-darwin-arm64.mcpb")?;
/// println!(
///     "{} files, {} bytes, signed: {}",
///     bundle.entries().len(),
///     bundle.size(),
///     bundle.is_signed(),
/// );
/// # Ok::<(), mcpb::Error>(())
/// ```
#[derive(Debug)]
pub struct Bundle {
    path: PathBuf,
    size: u64,
    manifest: Value,
    entries: Vec<Entry>,
    signed: bool,
}

/// A file of a [`Bundle`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its name in the zip, such as `server/my-server`.
    pub name: String,
    /// How many bytes it unpacks to.
    pub size: u64,
    /// Whether it is written as a program (mode 755) rather than as a file.
    pub executable: bool,
}

impl Bundle {
    /// Reads the bundle at `path`.
    ///
    /// A file that is not a zip, or a zip without the `manifest.json` a bundle
    /// must have, is refused rather than reported as an empty bundle.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = fs::File::open(&path).map_err(|error| Error::read(&path, error))?;
        let size = file
            .metadata()
            .map_err(|error| Error::read(&path, error))?
            .len();
        let signed = self::signed(&path)?;

        let mut archive = zip::ZipArchive::new(file).map_err(|error| Error::Bundle {
            path: path.clone(),
            reason: error.to_string(),
        })?;

        let manifest = read_manifest(&mut archive, &path)?;

        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let file = archive.by_index(index).map_err(|error| Error::Bundle {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            if file.is_dir() {
                continue;
            }
            entries.push(Entry {
                name: file.name().to_string(),
                size: file.size(),
                executable: file.unix_mode().is_some_and(|mode| mode & 0o111 != 0),
            });
        }

        Ok(Self {
            path,
            size,
            manifest,
            entries,
            signed,
        })
    }

    /// Where the bundle is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `manifest.json` of the bundle, as it was written.
    pub fn manifest(&self) -> &Value {
        &self.manifest
    }

    /// The manifest format the bundle declares, such as `0.3`.
    pub fn manifest_version(&self) -> Option<&str> {
        self.manifest["manifest_version"].as_str()
    }

    /// Every file of the bundle, `manifest.json` included.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// What the manifest points at as the server's entry point, such as
    /// `server/my-server`.
    pub fn entry_point(&self) -> Option<&str> {
        self.manifest["server"]["entry_point"].as_str()
    }

    /// How large the file is on disk, in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether the bundle carries a signature after its zip, which this crate
    /// reports but does not check: sign with the `mcpb` CLI and verify there.
    pub fn is_signed(&self) -> bool {
        self.signed
    }

    /// Whether the manifest is written in the format this crate writes.
    pub fn is_current(&self) -> bool {
        self.manifest_version() == Some(MANIFEST_VERSION)
    }

    /// Writes the bundle's files into `dir`, which is created. A file that
    /// unpacks to a program keeps the mode it was packed with.
    ///
    /// A name that would leave `dir`, such as `../x`, is refused rather than
    /// written.
    pub fn extract_to(&self, dir: impl AsRef<Path>) -> Result<(), Error> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(Error::write)?;

        let file = fs::File::open(&self.path).map_err(|error| Error::read(&self.path, error))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|error| Error::Bundle {
            path: self.path.clone(),
            reason: error.to_string(),
        })?;

        for entry in &self.entries {
            let target = inside(dir, &entry.name)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(Error::write)?;
            }

            let mut source = archive
                .by_name(&entry.name)
                .map_err(|error| Error::Bundle {
                    path: self.path.clone(),
                    reason: error.to_string(),
                })?;
            let mut unpacked = fs::File::create(&target).map_err(Error::write)?;
            io::copy(&mut source, &mut unpacked).map_err(Error::write)?;

            #[cfg(unix)]
            if entry.executable {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
                    .map_err(Error::write)?;
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for Bundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} ({} client{} behind it)",
            self.manifest["name"].as_str().unwrap_or("a bundle"),
            self.manifest["version"].as_str().unwrap_or("?"),
            self.entries.len(),
            if self.entries.len() == 1 { "" } else { "s" },
        )
    }
}

/// Whether the file ends with the marker of a signed bundle.
fn signed(path: &Path) -> Result<bool, Error> {
    use std::io::{Seek, SeekFrom};

    let mut file = fs::File::open(path).map_err(|error| Error::read(path, error))?;
    let size = file
        .metadata()
        .map_err(|error| Error::read(path, error))?
        .len();
    if size < SIGNATURE_END.len() as u64 {
        return Ok(false);
    }

    file.seek(SeekFrom::End(-(SIGNATURE_END.len() as i64)))
        .map_err(|error| Error::read(path, error))?;
    let mut end = [0; SIGNATURE_END.len()];
    file.read_exact(&mut end)
        .map_err(|error| Error::read(path, error))?;
    Ok(end == *SIGNATURE_END)
}

fn read_manifest<R: io::Read + io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &Path,
) -> Result<Value, Error> {
    let mut file = archive
        .by_name("manifest.json")
        .map_err(|_| Error::Bundle {
            path: path.to_path_buf(),
            reason: "it holds no manifest.json".to_string(),
        })?;
    let mut json = String::new();
    file.read_to_string(&mut json)
        .map_err(|error| Error::Bundle {
            path: path.to_path_buf(),
            reason: format!("manifest.json is not UTF-8: {error}"),
        })?;

    match serde_json::from_str(&json) {
        Ok(manifest @ Value::Object(_)) => Ok(manifest),
        Ok(_) => Err(Error::Bundle {
            path: path.to_path_buf(),
            reason: "manifest.json holds no JSON object".to_string(),
        }),
        Err(error) => Err(Error::Bundle {
            path: path.to_path_buf(),
            reason: format!("manifest.json is not JSON: {error}"),
        }),
    }
}

/// `name` inside `dir`, unless it points out of it.
fn inside(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    let path = Path::new(name);
    let escapes = name.is_empty()
        || path.is_absolute()
        || name.contains('\\')
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)));
    if escapes {
        return Err(Error::Unsafe(name.to_string()));
    }

    Ok(dir.join(path))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use super::*;
    use crate::{Author, BundleSpec, Executable, Platform};

    /// A bundle of one Linux binary, packed into `dir`, and the directory.
    fn packed() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = dir.path().join("server-binary");
        fs::write(&binary, b"\x7fELF fake").unwrap();
        fs::write(dir.path().join("manifest.json"), b"not the bundle's").unwrap();

        let spec = BundleSpec {
            name: "test-server".to_string(),
            version: "1.2.3".parse().unwrap(),
            description: "A test server.".to_string(),
            author: Author::new("Test"),
            executables: BTreeMap::from([(Platform::Linux, Executable::new(&binary, "arm64"))]),
            ..BundleSpec::default()
        };

        let bundle = dir.path().join(spec.file_name());
        spec.pack(&bundle).unwrap();
        (dir, bundle)
    }

    #[test]
    fn what_the_bundle_holds_comes_back() {
        let (_dir, path) = packed();
        let bundle = Bundle::open(&path).unwrap();

        assert_eq!(bundle.manifest()["name"], "test-server");
        assert_eq!(bundle.manifest_version(), Some(MANIFEST_VERSION));
        assert!(bundle.is_current());
        assert_eq!(bundle.entry_point(), Some("server/test-server"));
        assert_eq!(bundle.size(), fs::metadata(&path).unwrap().len());
        assert!(!bundle.is_signed());

        let entries = bundle.entries();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["manifest.json", "server/test-server"]
        );
        let server = &entries[1];
        assert!(server.executable, "{server:?}");
        assert_eq!(server.size, b"\x7fELF fake".len() as u64);
        assert!(!entries[0].executable);
    }

    #[test]
    fn the_files_come_out_where_the_manifest_says() {
        let (dir, path) = packed();
        let bundle = Bundle::open(&path).unwrap();

        let unpacked = dir.path().join("unpacked");
        bundle.extract_to(&unpacked).unwrap();

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(unpacked.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest, *bundle.manifest());
        assert!(unpacked.join(bundle.entry_point().unwrap()).is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(unpacked.join("server/test-server"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "{mode:o}");
        }
    }

    #[test]
    fn a_signature_after_the_zip_is_reported() {
        let (dir, path) = packed();
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(b"MCPB_SIG_V1");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"DER");
        bytes.extend_from_slice(SIGNATURE_END);

        let signed_path = dir.path().join("signed.mcpb");
        fs::write(&signed_path, &bytes).unwrap();

        let bundle = Bundle::open(&signed_path).unwrap();
        assert!(bundle.is_signed());
        // The zip the signature is appended to still reads.
        assert_eq!(bundle.manifest()["name"], "test-server");
    }

    #[test]
    fn a_file_that_is_no_bundle_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("plain.mcpb");
        fs::write(&path, b"just some bytes").unwrap();

        let error = Bundle::open(&path).unwrap_err();
        assert!(matches!(error, Error::Bundle { .. }), "{error}");

        let missing = Bundle::open(dir.path().join("nowhere.mcpb")).unwrap_err();
        assert!(matches!(missing, Error::Read { .. }), "{missing}");
    }

    #[test]
    fn a_zip_without_a_manifest_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty.mcpb");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("other.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let error = Bundle::open(&path).unwrap_err();
        assert!(error.to_string().contains("no manifest.json"), "{error}");
    }

    #[test]
    fn a_file_named_outside_the_directory_is_not_unpacked() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("evil.mcpb");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let plain = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", plain).unwrap();
        zip.write_all(br#"{"manifest_version": "0.3"}"#).unwrap();
        zip.start_file("../evil", plain).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();

        let bundle = Bundle::open(&path).unwrap();
        let unpacked = dir.path().join("unpacked");
        let error = bundle.extract_to(&unpacked).unwrap_err();
        assert!(matches!(error, Error::Unsafe(_)), "{error}");
        assert!(error.to_string().contains("../evil"), "{error}");
        assert!(!dir.path().join("evil").exists());
    }
}
