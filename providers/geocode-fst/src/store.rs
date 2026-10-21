//! Segment files: plain little-endian arrays and byte blobs, memory-mapped when read.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use bytemuck::Pod;
use geko_core::{GekoError, Result};
use memmap2::{Mmap, MmapOptions};

const _: () = assert!(cfg!(target_endian = "little"), "segment arrays are little-endian");

/// A read-only memory-mapped file. Empty files cannot be mapped, so they are kept as empty.
pub enum Bytes {
    Mapped(Mmap),
    Empty,
}

impl Bytes {
    pub fn open(path: &Path, preload: bool) -> Result<Self> {
        let file = File::open(path).map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
        if file.metadata()?.len() == 0 {
            return Ok(Bytes::Empty);
        }
        let mut options = MmapOptions::new();
        if preload {
            options.populate();
        }
        // SAFETY: segment files are immutable once written (PROJECT.md §12); nothing truncates
        // or rewrites them while mapped.
        let map = unsafe { options.map(&file) }.map_err(|e| GekoError::Data(format!("{}: {e}", path.display())))?;
        Ok(Bytes::Mapped(map))
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Empty => &[],
        }
    }

    pub fn slice<T: Pod>(&self) -> &[T] {
        match self {
            Bytes::Mapped(m) => bytemuck::cast_slice(m),
            Bytes::Empty => &[],
        }
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

pub fn write_array<T: Pod>(path: &Path, values: &[T]) -> Result<()> {
    write_bytes(path, bytemuck::cast_slice(values))
}

pub fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut w = BufWriter::new(File::create(&tmp)?);
    w.write_all(bytes)?;
    w.into_inner().map_err(|e| GekoError::Io(e.into_error()))?.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Variable-length byte records: concatenated data plus `n + 1` offsets.
#[derive(Default)]
pub struct Blobs {
    pub data: Vec<u8>,
    pub offsets: Vec<u64>,
}

impl Blobs {
    pub fn push(&mut self, bytes: &[u8]) {
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        self.data.extend_from_slice(bytes);
        self.offsets.push(self.data.len() as u64);
    }

    pub fn write(&self, dir: &Path, name: &str) -> Result<()> {
        write_bytes(&dir.join(format!("{name}.bin")), &self.data)?;
        write_array(&dir.join(format!("{name}.idx")), if self.offsets.is_empty() { &[0] } else { &self.offsets })
    }
}

/// `Blobs` written to disk as they are pushed: only the offsets stay in memory.
pub struct BlobWriter {
    path: PathBuf,
    idx: PathBuf,
    data: BufWriter<File>,
    offsets: Vec<u64>,
}

impl BlobWriter {
    pub fn create(dir: &Path, name: &str) -> Result<Self> {
        let path = dir.join(format!("{name}.bin"));
        let data = BufWriter::new(File::create(path.with_extension("tmp"))?);
        Ok(Self { idx: dir.join(format!("{name}.idx")), path, data, offsets: vec![0] })
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.data.write_all(bytes)?;
        self.offsets.push(self.offsets[self.offsets.len() - 1] + bytes.len() as u64);
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.data.into_inner().map_err(|e| GekoError::Io(e.into_error()))?.sync_all()?;
        fs::rename(self.path.with_extension("tmp"), &self.path)?;
        write_array(&self.idx, &self.offsets)
    }
}

pub struct BlobReader {
    data: Bytes,
    offsets: Bytes,
}

impl BlobReader {
    pub fn open(dir: &Path, name: &str, preload: bool) -> Result<Self> {
        Ok(Self {
            data: Bytes::open(&dir.join(format!("{name}.bin")), preload)?,
            offsets: Bytes::open(&dir.join(format!("{name}.idx")), preload)?,
        })
    }

    pub fn get(&self, i: usize) -> &[u8] {
        let offsets: &[u64] = self.offsets.slice();
        match (offsets.get(i), offsets.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.data.bytes()[a as usize..b as usize],
            _ => &[],
        }
    }

    pub fn str(&self, i: usize) -> &str {
        std::str::from_utf8(self.get(i)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_and_blobs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        write_array(&dir.path().join("a.bin"), &[1u32, 2, 3]).unwrap();
        assert_eq!(Bytes::open(&dir.path().join("a.bin"), false).unwrap().slice::<u32>(), &[1, 2, 3]);
        write_array::<u32>(&dir.path().join("e.bin"), &[]).unwrap();
        assert!(Bytes::open(&dir.path().join("e.bin"), false).unwrap().slice::<u32>().is_empty());

        let mut blobs = Blobs::default();
        blobs.push(b"hola");
        blobs.push(b"");
        blobs.push("Carrer de Mallorca".as_bytes());
        blobs.write(dir.path(), "names").unwrap();
        let r = BlobReader::open(dir.path(), "names", false).unwrap();
        assert_eq!((r.str(0), r.str(1), r.str(2), r.str(3)), ("hola", "", "Carrer de Mallorca", ""));

        let mut w = BlobWriter::create(dir.path(), "streamed").unwrap();
        for b in [&b"hola"[..], b"", "Carrer de Mallorca".as_bytes()] {
            w.push(b).unwrap();
        }
        w.finish().unwrap();
        for ext in ["bin", "idx"] {
            assert_eq!(fs::read(dir.path().join(format!("streamed.{ext}"))).unwrap(), fs::read(dir.path().join(format!("names.{ext}"))).unwrap());
        }
        BlobWriter::create(dir.path(), "none").unwrap().finish().unwrap();
        Blobs::default().write(dir.path(), "empty").unwrap();
        assert_eq!(fs::read(dir.path().join("none.idx")).unwrap(), fs::read(dir.path().join("empty.idx")).unwrap());
    }
}
