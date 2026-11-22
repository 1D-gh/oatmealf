use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub executable_name: String,
    pub executable_path: Option<PathBuf>,
}

impl ProcessInfo {
    pub fn process_name(&self) -> String {
        Path::new(&self.executable_name)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub base: u64,
    pub size: u64,
    pub image: bool,
}

pub fn editor_engine_root(editor: &ProcessInfo) -> Result<PathBuf> {
    let executable = editor.executable_path.as_ref().with_context(
        || "could not read the UnrealEditor executable path; pass --engine-root explicitly",
    )?;
    let engine_directory = executable
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .with_context(|| {
            format!(
                "the editor executable is not under Engine\\Binaries\\Win64: {}",
                executable.display()
            )
        })?;
    crate::planner::normalize_engine_root(engine_directory).with_context(|| {
        format!(
            "the running editor does not point to an engine tree containing source ({})",
            executable.display()
        )
    })
}

#[cfg(windows)]
mod platform {
    use super::{MemoryRegion, ProcessInfo};
    use anyhow::{Context, Result, bail};
    use std::ffi::c_void;
    use std::io;
    use std::mem::size_of;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_IMAGE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
        PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
        VirtualQueryEx,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        QueryFullProcessImageNameW,
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    pub struct LiveMemory {
        handle: OwnedHandle,
    }

    impl LiveMemory {
        pub fn open(process_id: u32) -> Result<Self> {
            let handle =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, process_id) };
            if handle.is_null() {
                return Err(io::Error::last_os_error()).with_context(|| {
                    format!("could not open process {process_id} for read-only capture")
                });
            }
            Ok(Self {
                handle: OwnedHandle(handle),
            })
        }

        pub fn read_bytes(&self, address: u64, size: usize) -> Option<Vec<u8>> {
            if size == 0 {
                return Some(Vec::new());
            }
            let mut buffer = vec![0u8; size];
            self.read_into(address, &mut buffer).then_some(buffer)
        }

        pub fn read_array<const N: usize>(&self, address: u64) -> Option<[u8; N]> {
            let mut buffer = [0u8; N];
            self.read_into(address, &mut buffer).then_some(buffer)
        }

        fn read_into(&self, address: u64, buffer: &mut [u8]) -> bool {
            if buffer.is_empty() {
                return true;
            }
            let mut bytes_read = 0usize;
            let success = unsafe {
                ReadProcessMemory(
                    self.handle.0,
                    address as usize as *const c_void,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut bytes_read,
                )
            };
            success != 0 && bytes_read == buffer.len()
        }

        pub fn read_pointer(&self, address: u64) -> Option<u64> {
            self.read_array(address).map(u64::from_le_bytes)
        }

        pub fn read_u32(&self, address: u64) -> Result<u32> {
            let bytes = self
                .read_array(address)
                .with_context(|| format!("could not read uint32 at 0x{address:X}"))?;
            Ok(u32::from_le_bytes(bytes))
        }

        pub fn readable_regions(&self) -> Vec<MemoryRegion> {
            let mut regions = Vec::new();
            let mut address = 0x1_0000u64;
            while address < 0x0000_7fff_ffff_ffff {
                let mut info = MEMORY_BASIC_INFORMATION::default();
                let result = unsafe {
                    VirtualQueryEx(
                        self.handle.0,
                        address as usize as *const c_void,
                        &mut info,
                        size_of::<MEMORY_BASIC_INFORMATION>(),
                    )
                };
                if result == 0 {
                    break;
                }
                let base = info.BaseAddress as usize as u64;
                let size = info.RegionSize as u64;
                let basic_protection = info.Protect & 0xff;
                // pages can still change before ReadProcessMemory runs
                let readable = matches!(
                    basic_protection,
                    PAGE_READONLY
                        | PAGE_READWRITE
                        | PAGE_WRITECOPY
                        | PAGE_EXECUTE_READ
                        | PAGE_EXECUTE_READWRITE
                        | PAGE_EXECUTE_WRITECOPY
                );
                if info.State == MEM_COMMIT
                    && readable
                    && info.Protect & PAGE_GUARD == 0
                    && size > 0
                {
                    regions.push(MemoryRegion {
                        base,
                        size,
                        image: info.Type == MEM_IMAGE,
                    });
                }
                let next = base.saturating_add(size);
                if next <= address {
                    break;
                }
                address = next;
            }
            regions
        }
    }

    pub fn find_exactly_one(executable_name: &str, label: &str) -> Result<ProcessInfo> {
        let executable_name = normalize_executable_name(executable_name);
        let matches: Vec<ProcessInfo> = enumerate_processes()?
            .into_iter()
            .filter(|process| {
                process
                    .executable_name
                    .eq_ignore_ascii_case(&executable_name)
            })
            .collect();
        match matches.as_slice() {
            [] => bail!(
                "no {label} process is running ({executable_name}); start it first or pass a saved --{label}-layouts"
            ),
            [process] => Ok(process.clone()),
            _ => bail!(
                "found {} {label} processes; leave exactly one running",
                matches.len()
            ),
        }
    }

    pub fn find_shipping_game(requested_name: Option<&str>) -> Result<ProcessInfo> {
        if let Some(name) = requested_name {
            return find_exactly_one(name, "game");
        }

        let matches: Vec<ProcessInfo> = enumerate_processes()?
            .into_iter()
            .filter(|process| {
                process
                    .executable_name
                    .to_ascii_lowercase()
                    .ends_with("-win64-shipping.exe")
            })
            .collect();
        match matches.as_slice() {
            [] => bail!(
                "no Unreal Engine shipping game was found; start the game or pass --game <process>"
            ),
            [process] => Ok(process.clone()),
            _ => {
                let names = matches
                    .iter()
                    .map(|process| format!("{} ({})", process.executable_name, process.pid))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("found multiple shipping games ({names}); select one with --game <process>")
            }
        }
    }

    fn normalize_executable_name(value: &str) -> String {
        if value.to_ascii_lowercase().ends_with(".exe") {
            value.to_string()
        } else {
            format!("{value}.exe")
        }
    }

    fn enumerate_processes() -> Result<Vec<ProcessInfo>> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error()).context("could not enumerate processes");
        }
        let snapshot = OwnedHandle(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
            return Err(io::Error::last_os_error()).context("could not read process snapshot");
        }

        let mut processes = Vec::new();
        loop {
            let name_length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let executable_name = String::from_utf16_lossy(&entry.szExeFile[..name_length]);
            processes.push(ProcessInfo {
                pid: entry.th32ProcessID,
                executable_name,
                executable_path: query_executable_path(entry.th32ProcessID),
            });
            if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
                break;
            }
        }
        Ok(processes)
    }

    fn query_executable_path(process_id: u32) -> Option<PathBuf> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if handle.is_null() {
            return None;
        }
        let handle = OwnedHandle(handle);
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        let success =
            unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) };
        if success == 0 {
            return None;
        }
        buffer.truncate(length as usize);
        Some(PathBuf::from(String::from_utf16_lossy(&buffer)))
    }
}

#[cfg(windows)]
pub use platform::{LiveMemory, find_exactly_one, find_shipping_game};

#[cfg(not(windows))]
pub struct LiveMemory;

#[cfg(not(windows))]
impl LiveMemory {
    pub fn open(_process_id: u32) -> Result<Self> {
        anyhow::bail!("live shader capture is only supported on 64-bit Windows")
    }

    pub fn read_bytes(&self, _address: u64, _size: usize) -> Option<Vec<u8>> {
        None
    }

    pub fn read_pointer(&self, _address: u64) -> Option<u64> {
        None
    }

    pub fn read_u32(&self, _address: u64) -> Result<u32> {
        anyhow::bail!("live shader capture is only supported on 64-bit Windows")
    }

    pub fn readable_regions(&self) -> Vec<MemoryRegion> {
        Vec::new()
    }
}

#[cfg(not(windows))]
pub fn find_exactly_one(_executable_name: &str, _label: &str) -> Result<ProcessInfo> {
    anyhow::bail!("live shader capture is only supported on 64-bit Windows")
}

#[cfg(not(windows))]
pub fn find_shipping_game(_requested_name: Option<&str>) -> Result<ProcessInfo> {
    anyhow::bail!("live shader capture is only supported on 64-bit Windows")
}

/// Memory source for the capture code: a live process or a Windows minidump.
pub enum ProcessMemory {
    Live(LiveMemory),
    Dump(crate::minidump::DumpMemory),
}

impl ProcessMemory {
    pub fn open(process_id: u32) -> Result<Self> {
        Ok(Self::Live(LiveMemory::open(process_id)?))
    }

    pub fn read_bytes(&self, address: u64, size: usize) -> Option<Vec<u8>> {
        match self {
            Self::Live(memory) => memory.read_bytes(address, size),
            Self::Dump(memory) => memory.read_bytes(address, size),
        }
    }

    pub fn read_pointer(&self, address: u64) -> Option<u64> {
        let bytes = self.read_bytes(address, 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    pub fn read_u32(&self, address: u64) -> Result<u32> {
        let bytes = self
            .read_bytes(address, 4)
            .with_context(|| format!("could not read uint32 at 0x{address:X}"))?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    pub fn readable_regions(&self) -> Vec<MemoryRegion> {
        match self {
            Self::Live(memory) => memory.readable_regions(),
            Self::Dump(memory) => memory.regions(),
        }
    }
}
