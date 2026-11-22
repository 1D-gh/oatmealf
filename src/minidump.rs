//! Minimal Windows minidump (MDMP) reader: module list, system info and the
//! Memory64List stream (full-memory dumps). Memory is read lazily from the file.

use crate::process::MemoryRegion;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::path::Path;

const STREAM_MODULE_LIST: u32 = 4;
const STREAM_MEMORY64_LIST: u32 = 9;
const STREAM_SYSTEM_INFO: u32 = 7;
const STREAM_MISC_INFO: u32 = 15;

#[derive(Clone, Debug)]
pub struct DumpModule {
    pub base: u64,
    pub size: u32,
    pub name: String,
    pub file_version: (u16, u16, u16, u16),
    pub product_version: (u16, u16, u16, u16),
}

#[derive(Clone, Copy, Debug)]
struct Region {
    base: u64,
    size: u64,
    file_offset: u64,
}

pub struct DumpMemory {
    file: File,
    regions: Vec<Region>, // sorted by base
    pub modules: Vec<DumpModule>,
    pub processor_architecture: u16,
    pub process_id: u32,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn read_at(file: &File, offset: u64, size: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; size];
    if !read_exact_at(file, offset, &mut buffer) {
        bail!("minidump is truncated at file offset 0x{offset:X} (+{size})");
    }
    Ok(buffer)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> bool {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        match file.seek_read(buffer, offset) {
            Ok(0) | Err(_) => return false,
            Ok(n) => {
                offset += n as u64;
                buffer = &mut buffer[n..];
            }
        }
    }
    true
}

#[cfg(not(windows))]
fn read_exact_at(file: &File, offset: u64, buffer: &mut [u8]) -> bool {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buffer, offset).is_ok()
}

impl DumpMemory {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let header = read_at(&file, 0, 32)?;
        if &header[0..4] != b"MDMP" {
            bail!(
                "{} is not a minidump (missing MDMP signature)",
                path.display()
            );
        }
        let stream_count = u32_at(&header, 8) as usize;
        let directory_rva = u64::from(u32_at(&header, 12));
        let directory = read_at(&file, directory_rva, stream_count * 12)?;
        let mut modules_stream = None;
        let mut memory_stream = None;
        let mut system_stream = None;
        let mut misc_stream = None;
        for index in 0..stream_count {
            let kind = u32_at(&directory, index * 12);
            let size = u64::from(u32_at(&directory, index * 12 + 4));
            let rva = u64::from(u32_at(&directory, index * 12 + 8));
            match kind {
                STREAM_MODULE_LIST => modules_stream = Some((rva, size)),
                STREAM_MEMORY64_LIST => memory_stream = Some((rva, size)),
                STREAM_SYSTEM_INFO => system_stream = Some((rva, size)),
                STREAM_MISC_INFO => misc_stream = Some((rva, size)),
                _ => {}
            }
        }
        let (memory_rva, _) = memory_stream
            .context("minidump has no Memory64List stream (not a full-memory dump)")?;
        let head = read_at(&file, memory_rva, 16)?;
        let count = u64_at(&head, 0) as usize;
        let mut data_offset = u64_at(&head, 8);
        let descriptors = read_at(&file, memory_rva + 16, count * 16)?;
        let mut regions = Vec::with_capacity(count);
        for index in 0..count {
            let base = u64_at(&descriptors, index * 16);
            let size = u64_at(&descriptors, index * 16 + 8);
            regions.push(Region {
                base,
                size,
                file_offset: data_offset,
            });
            data_offset += size;
        }
        regions.sort_by_key(|region| region.base);

        let mut modules = Vec::new();
        if let Some((rva, _)) = modules_stream {
            let count = u32_at(&read_at(&file, rva, 4)?, 0) as usize;
            let raw = read_at(&file, rva + 4, count * 108)?;
            for index in 0..count {
                let entry = &raw[index * 108..(index + 1) * 108];
                let name_rva = u64::from(u32_at(entry, 20));
                let name_len = u32_at(&read_at(&file, name_rva, 4)?, 0) as usize;
                let name_bytes = read_at(&file, name_rva + 4, name_len)?;
                let units: Vec<u16> = name_bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                // VS_FIXEDFILEINFO starts at +24: sig, struct ver, FileVerMS, FileVerLS, ProdVerMS, ProdVerLS
                let ver = |ms: u32, ls: u32| {
                    ((ms >> 16) as u16, ms as u16, (ls >> 16) as u16, ls as u16)
                };
                modules.push(DumpModule {
                    base: u64_at(entry, 0),
                    size: u32_at(entry, 8),
                    name: String::from_utf16_lossy(&units),
                    file_version: ver(u32_at(entry, 32), u32_at(entry, 36)),
                    product_version: ver(u32_at(entry, 40), u32_at(entry, 44)),
                });
            }
        }
        let processor_architecture = match system_stream {
            Some((rva, _)) => u16_at(&read_at(&file, rva, 2)?, 0),
            None => 0,
        };
        // MINIDUMP_MISC_INFO: SizeOfInfo, Flags1 (bit0 = ProcessId valid), ProcessId
        let process_id = match misc_stream {
            Some((rva, size)) if size >= 12 => {
                let raw = read_at(&file, rva, 12)?;
                if u32_at(&raw, 4) & 1 != 0 { u32_at(&raw, 8) } else { 0 }
            }
            _ => 0,
        };
        Ok(Self {
            file,
            process_id,
            regions,
            modules,
            processor_architecture,
        })
    }

    pub fn total_memory(&self) -> u64 {
        self.regions.iter().map(|region| region.size).sum()
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    /// The first listed module (the executable).
    pub fn main_module(&self) -> Option<&DumpModule> {
        self.modules.first()
    }

    /// Regions in the shape the capture code expects. A region is "image" when it lies in a module.
    pub fn regions(&self) -> Vec<MemoryRegion> {
        let mut ranges: Vec<(u64, u64)> = self
            .modules
            .iter()
            .map(|module| (module.base, module.base + u64::from(module.size)))
            .collect();
        ranges.sort();
        self.regions
            .iter()
            .map(|region| {
                let index = ranges.partition_point(|range| range.0 <= region.base);
                let image = index > 0 && region.base < ranges[index - 1].1;
                MemoryRegion {
                    base: region.base,
                    size: region.size,
                    image,
                }
            })
            .collect()
    }

    pub fn read_bytes(&self, address: u64, size: usize) -> Option<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        let mut done = 0usize;
        while done < size {
            let current = address.checked_add(done as u64)?;
            let index = self
                .regions
                .partition_point(|region| region.base <= current)
                .checked_sub(1)?;
            let region = self.regions[index];
            let within = current - region.base;
            if within >= region.size {
                return None;
            }
            let take = ((region.size - within) as usize).min(size - done);
            if !read_exact_at(
                &self.file,
                region.file_offset + within,
                &mut buffer[done..done + take],
            ) {
                return None;
            }
            done += take;
        }
        Some(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tiny_dump() -> Vec<u8> {
        // header(32) + 1 directory entry(12) + memory64 stream(16 + 2*16) + data
        let mut b = Vec::new();
        b.extend_from_slice(b"MDMP");
        b.extend_from_slice(&0xa793u32.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&32u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]);
        b.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(b.len(), 32);
        let stream_rva = 44u32;
        b.extend_from_slice(&STREAM_MEMORY64_LIST.to_le_bytes());
        b.extend_from_slice(&48u32.to_le_bytes());
        b.extend_from_slice(&stream_rva.to_le_bytes());
        let data_rva = 44u64 + 16 + 32;
        b.extend_from_slice(&2u64.to_le_bytes());
        b.extend_from_slice(&data_rva.to_le_bytes());
        b.extend_from_slice(&0x1000u64.to_le_bytes());
        b.extend_from_slice(&4u64.to_le_bytes());
        b.extend_from_slice(&0x1004u64.to_le_bytes()); // adjacent region
        b.extend_from_slice(&4u64.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        b
    }

    #[test]
    fn reads_across_adjacent_regions() {
        let path = std::env::temp_dir().join(format!("oatmealf-test-{}.dmp", std::process::id()));
        File::create(&path)
            .unwrap()
            .write_all(&tiny_dump())
            .unwrap();
        let dump = DumpMemory::open(&path).unwrap();
        assert_eq!(dump.region_count(), 2);
        assert_eq!(dump.total_memory(), 8);
        assert_eq!(dump.read_bytes(0x1002, 4).unwrap(), [3, 4, 5, 6]);
        assert!(dump.read_bytes(0x1006, 4).is_none());
        assert!(dump.read_bytes(0x0fff, 2).is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_non_minidump() {
        let path = std::env::temp_dir().join(format!("oatmealf-bad-{}.dmp", std::process::id()));
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        assert!(DumpMemory::open(&path).is_err());
        std::fs::remove_file(path).ok();
    }
}
